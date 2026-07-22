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

use application::calendar::{CalendarPreview, CalendarTarget};
use application::external_operation::{
    ExternalResourceId, FailureCode, OperationFailure, ProviderOutcome, SanitizedSummary,
};
use domain::confirmation::{ConfirmationAction, ConfirmationRecord, ConfirmationStatus};
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;

use crate::auth::{GoogleAccessToken, OwnerTokenSource};

// ── bounded value types ───────────────────────────────────────────────

const MAX_TITLE_BYTES: usize = 256;
const MAX_TIMEZONE_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 2048;
const MAX_CALENDAR_ID_BYTES: usize = 256;
const MAX_EMAIL_BYTES: usize = 320;
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

/// An RFC3339 event boundary validated by [`CalendarPreview`].
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
    /// Build the provider request only from the exact canonical preview shown
    /// to participants. Callers cannot supply an independent fingerprint.
    pub fn from_preview(preview: &CalendarPreview) -> Result<Self, CalendarError> {
        let calendar = match preview.calendar() {
            CalendarTarget::Primary => CalendarSelection::Primary,
            CalendarTarget::Alternate { calendar_id } => {
                CalendarSelection::Alternate(CalendarId::new(calendar_id.clone())?)
            }
        };
        let attendees = preview
            .attendees()
            .iter()
            .map(|email| AttendeeEmail::new(email.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let reminders = ReminderSettings::new(
            preview.reminders().push_minutes,
            preview.reminders().email_minutes,
        )?;
        let mutation_target = preview
            .mutation_target()
            .map_err(|_| CalendarError::PreviewBinding)?;
        Ok(Self {
            title: EventTitle::new(preview.title().to_owned())?,
            start: EventTimestamp::new(preview.start().to_owned())?,
            end: EventTimestamp::new(preview.end().to_owned())?,
            timezone: TimezoneLabel::new(preview.timezone().to_owned())?,
            calendar,
            description: preview
                .description()
                .map(|value| EventDescription::new(value.to_owned()))
                .transpose()?,
            attendees,
            reminders,
            send_invitations: preview.send_invitations(),
            target_fingerprint: OperationTargetFingerprint::new(*mutation_target.as_bytes()),
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
/// `RetryableFailure` → the client proves the request was not accepted, so a
/// retry cannot duplicate an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarProviderOutcome {
    Created(ExternalResourceId),
    Ambiguous,
    Terminal,
    RetryableFailure,
}

/// Provider access decision for an explicitly selected alternate calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarAccess {
    Writable,
    Denied,
    RetryableFailure,
}

/// Google Calendar provider client boundary.
///
/// Implementations translate the validated [`CalendarEventRequest`] into a
/// Calendar API insert call using the workflow owner's access token. They must
/// not recompute or mutate any extracted value. Alternate calendars are
/// checked for writable access before insertion.
#[allow(async_fn_in_trait)]
pub trait GoogleCalendarClient {
    async fn calendar_access(
        &self,
        token: &GoogleAccessToken,
        calendar: &CalendarId,
    ) -> CalendarAccess;

    /// Invoke event insertion and explicitly classify all transport failures.
    /// Timeouts or disconnects after a request may have been accepted must be
    /// `Ambiguous`; only failures known to occur before acceptance may be
    /// `RetryableFailure`.
    async fn create_event(
        &self,
        token: &GoogleAccessToken,
        request: &CalendarEventRequest,
    ) -> CalendarProviderOutcome;
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
    #[error("calendar preview could not be bound to an operation target")]
    PreviewBinding,
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

    /// Resolve the workflow owner's token, verify alternate-calendar access,
    /// then create the event. No arbitrary token can be supplied by callers.
    ///
    /// `Accepted` carries the stable event resource id. `Ambiguous` and
    /// `TerminalFailure` carry sanitized failure metadata only — never the
    /// request payload, attendee emails, or token.
    pub async fn create_event<TokenSource>(
        &self,
        token_source: &TokenSource,
        proof: &ConfirmedCalendarProof,
        request: &CalendarEventRequest,
    ) -> Result<ProviderOutcome, CreateEventError>
    where
        TokenSource: OwnerTokenSource,
    {
        Self::validate(proof, request)?;
        let token = match token_source.access_token(proof.owner()).await {
            Ok(token) => token,
            Err(_) => {
                let f = failure(
                    "google_owner_token_unavailable",
                    "workflow owner google token is unavailable",
                )?;
                return Ok(ProviderOutcome::RetryableFailure(f));
            }
        };

        if let CalendarSelection::Alternate(calendar) = request.calendar() {
            match self.client.calendar_access(&token, calendar).await {
                CalendarAccess::Writable => {}
                CalendarAccess::Denied => {
                    let f = failure(
                        "google_calendar_access_denied",
                        "workflow owner cannot write to selected calendar",
                    )?;
                    return Ok(ProviderOutcome::TerminalFailure(f));
                }
                CalendarAccess::RetryableFailure => {
                    let f = failure(
                        "google_calendar_access_failed",
                        "selected calendar access check failed",
                    )?;
                    return Ok(ProviderOutcome::RetryableFailure(f));
                }
            }
        }

        match self.client.create_event(&token, request).await {
            CalendarProviderOutcome::Created(id) => Ok(ProviderOutcome::Accepted {
                resource_id: Some(id),
            }),
            CalendarProviderOutcome::Ambiguous => {
                let f = failure(
                    "google_calendar_ambiguous",
                    "google calendar outcome is ambiguous",
                )?;
                Ok(ProviderOutcome::Ambiguous(f))
            }
            CalendarProviderOutcome::Terminal => {
                let f = failure(
                    "google_calendar_terminal",
                    "google calendar permanently rejected the event",
                )?;
                Ok(ProviderOutcome::TerminalFailure(f))
            }
            CalendarProviderOutcome::RetryableFailure => {
                let f = failure(
                    "google_calendar_failed",
                    "google calendar request was not accepted",
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
    async fn calendar_access(
        &self,
        _token: &GoogleAccessToken,
        _calendar: &CalendarId,
    ) -> CalendarAccess {
        unreachable!("unit validate tests never invoke the client")
    }

    async fn create_event(
        &self,
        _token: &GoogleAccessToken,
        _request: &CalendarEventRequest,
    ) -> CalendarProviderOutcome {
        unreachable!("unit validate tests never invoke the client")
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use application::calendar::{CalendarReminders, CalendarTarget};

    use super::*;

    fn preview(calendar: CalendarTarget) -> CalendarPreview {
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
            true,
        )
        .expect("valid preview")
    }

    #[test]
    fn request_is_derived_from_exact_primary_preview() {
        let preview = preview(CalendarTarget::Primary);
        let request = CalendarEventRequest::from_preview(&preview).expect("request");
        assert_eq!(request.calendar(), &CalendarSelection::Primary);
        assert_eq!(request.title().as_str(), preview.title());
        assert_eq!(request.start().as_str(), preview.start());
        assert_eq!(request.end().as_str(), preview.end());
        assert_eq!(request.timezone().as_str(), preview.timezone());
        assert_eq!(request.attendees().len(), 2);
        assert!(request.send_invitations());
    }

    #[test]
    fn request_is_derived_from_exact_alternate_preview() {
        let preview = preview(CalendarTarget::Alternate {
            calendar_id: "work-calendar-123".to_owned(),
        });
        let request = CalendarEventRequest::from_preview(&preview).expect("request");
        assert!(matches!(
            request.calendar(),
            CalendarSelection::Alternate(id) if id.as_str() == "work-calendar-123"
        ));
    }

    #[test]
    fn changing_confirmed_payload_changes_operation_target() {
        let original = preview(CalendarTarget::Primary);
        let changed = CalendarPreview::new(
            original.title().to_owned(),
            original.start().to_owned(),
            original.end().to_owned(),
            original.timezone().to_owned(),
            CalendarTarget::Primary,
            original.description().map(str::to_owned),
            original.attendees().to_vec(),
            CalendarReminders {
                push_minutes: Some(10),
                email_minutes: None,
            },
            false,
        )
        .expect("changed preview");
        let original_request = CalendarEventRequest::from_preview(&original).expect("request");
        let changed_request = CalendarEventRequest::from_preview(&changed).expect("request");
        assert_ne!(
            original_request.target_fingerprint(),
            changed_request.target_fingerprint()
        );
    }

    #[test]
    fn validate_rejects_target_mismatch() {
        let request =
            CalendarEventRequest::from_preview(&preview(CalendarTarget::Primary)).expect("request");
        let proof = ConfirmedCalendarProof {
            owner: ParticipantId::new(101).unwrap(),
            mutation_target: MutationTargetFingerprint::new([9u8; 32]),
            preview_digest: PreviewDigest::new([1u8; 32]),
            workflow_revision: WorkflowRevision::new(2),
        };
        assert!(matches!(
            GoogleCalendarService::<()>::validate(&proof, &request),
            Err(CreateEventError::TargetMismatch)
        ));
    }

    #[test]
    fn validate_accepts_matching_target() {
        let preview = preview(CalendarTarget::Primary);
        let request = CalendarEventRequest::from_preview(&preview).expect("request");
        let mutation_target = preview.mutation_target().expect("target");
        let proof = ConfirmedCalendarProof {
            owner: ParticipantId::new(101).unwrap(),
            mutation_target,
            preview_digest: preview.digest().expect("digest"),
            workflow_revision: WorkflowRevision::new(2),
        };
        GoogleCalendarService::<()>::validate(&proof, &request).expect("target matches");
    }
}
