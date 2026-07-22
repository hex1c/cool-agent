//! Confirmed Calendar event creation orchestration (Task 36).
//!
//! Pure application layer that binds a consumed confirmation to the external
//! operation journal key for a Google Calendar event creation.  Like
//! [`crate::existing_files`], every method is pure — it operates over domain
//! types and returns only the validated binding.  Persistence
//! (`ConfirmationRepository::consume_and_prepare_operation`) and provider
//! invocation (`ExternalOperationExecutor` with the
//! [`google::calendar::GoogleCalendarService`]) are the caller's
//! responsibility.
//!
//! The binding enforces the Task 36 contract:
//! - The confirmation must be consumed with `StartCalendarOrEmailAction`.
//! - The operation key binds the event's mutation-target fingerprint via the
//!   confirmation's mutation-target fingerprint and the resulting workflow
//!   revision.
//! - Calendar creation uses [`OperationKind::GoogleWrite`] (email uses
//!   `SmtpSend` — see Task 37).
//! - A stale (already-consumed) confirmation is rejected by the domain
//!   `consume` step before this service is reached; a duplicate
//!   `consume_and_prepare_operation` is rejected by the repository conditional
//!   write.  In both cases zero provider writes occur, so reminders and event
//!   creations cannot duplicate on retry.

use std::collections::HashSet;
use std::fmt::{Display, Formatter};

use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use domain::authorization::AuthorizedWorkflowAction;
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumeRequest, ConfirmationConsumption,
    ConfirmationIssueOutcome, ConfirmationIssueRequest, ConfirmationRecord,
    MutationTargetFingerprint, PreviewDigest, TopicMessageReference,
};
use domain::contracts::CalendarResult;
use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
use domain::identity::{ConfirmationId, MessageId, ParticipantId};
use domain::retry::RetryPolicy;
use domain::workflow::{WaitDeadline, Workflow, WorkflowRevision, WorkflowTimestamp};

use crate::repositories::{ConsumeAndPrepareError, ConsumeAndPrepareRequest};

const CALENDAR_TARGET_LABEL: &str = "calendar-event-v2";
const MAX_TITLE_BYTES: usize = 256;
const MAX_TIMEZONE_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 2_048;
const MAX_CALENDAR_ID_BYTES: usize = 256;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_ATTENDEES: usize = 200;
const MAX_REMINDER_MINUTES: u16 = 40_320;

/// Calendar chosen in the confirmation preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CalendarTarget {
    /// The workflow owner's primary calendar.
    Primary,
    /// An explicitly selected alternate calendar. Provider access is checked
    /// with the workflow owner's token before event creation.
    Alternate { calendar_id: String },
}

/// Workflow-owner reminder settings shown in the confirmation preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarReminders {
    pub push_minutes: Option<u16>,
    pub email_minutes: Option<u16>,
}

/// Defaults loaded from the workflow owner's Google Calendar profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerCalendarDefaults {
    timezone: String,
    reminder_minutes: u16,
}

impl OwnerCalendarDefaults {
    pub fn new(timezone: String, reminder_minutes: u16) -> Result<Self, CalendarPreviewError> {
        validate_timezone(&timezone)?;
        if reminder_minutes > MAX_REMINDER_MINUTES {
            return Err(CalendarPreviewError::InvalidReminder);
        }
        Ok(Self {
            timezone,
            reminder_minutes,
        })
    }

    pub fn timezone(&self) -> &str {
        &self.timezone
    }

    pub const fn reminder_minutes(&self) -> u16 {
        self.reminder_minutes
    }
}

/// Canonical Calendar preview shown immediately before confirmation.
///
/// Every provider-relevant field is serialized into [`Self::digest`] and
/// [`Self::mutation_target`]. The Google adapter can only build a request from
/// this type, so attendees, invitation choice, calendar, timestamps, timezone,
/// description, and reminders cannot change after confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarPreview {
    title: String,
    start: String,
    end: String,
    timezone: String,
    calendar: CalendarTarget,
    description: Option<String>,
    attendees: Vec<String>,
    reminders: CalendarReminders,
    send_invitations: bool,
}

impl CalendarPreview {
    /// Resolve AI extraction into the confirmation preview. Missing timezone,
    /// calendar, and reminder data use workflow-owner defaults; an explicit
    /// calendar id selects an alternate calendar whose access is checked by
    /// the Google adapter before insertion.
    pub fn from_extraction(
        result: &CalendarResult,
        defaults: &OwnerCalendarDefaults,
    ) -> Result<Self, CalendarPreviewError> {
        let calendar = result
            .calendar_id
            .as_ref()
            .map(|calendar_id| CalendarTarget::Alternate {
                calendar_id: calendar_id.clone(),
            })
            .unwrap_or(CalendarTarget::Primary);
        let reminders = result
            .reminders
            .as_ref()
            .map(|value| CalendarReminders {
                push_minutes: value.push_minutes,
                email_minutes: value.email_minutes,
            })
            .unwrap_or(CalendarReminders {
                push_minutes: Some(defaults.reminder_minutes()),
                email_minutes: None,
            });
        Self::new(
            result.title.clone(),
            result.start.clone(),
            result.end.clone(),
            result
                .timezone
                .clone()
                .unwrap_or_else(|| defaults.timezone().to_owned()),
            calendar,
            result.description.clone(),
            result.attendees.clone(),
            reminders,
            result.send_invitations,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        title: String,
        start: String,
        end: String,
        timezone: String,
        calendar: CalendarTarget,
        description: Option<String>,
        mut attendees: Vec<String>,
        reminders: CalendarReminders,
        send_invitations: bool,
    ) -> Result<Self, CalendarPreviewError> {
        validate_bounded_text(&title, MAX_TITLE_BYTES, CalendarPreviewError::InvalidTitle)?;
        validate_timezone(&timezone)?;
        validate_calendar(&calendar)?;
        if let Some(value) = description.as_deref()
            && (value.len() > MAX_DESCRIPTION_BYTES || value.chars().any(char::is_control))
        {
            return Err(CalendarPreviewError::InvalidDescription);
        }
        let start_value = OffsetDateTime::parse(&start, &Rfc3339)
            .map_err(|_| CalendarPreviewError::InvalidTimestamp)?;
        let end_value = OffsetDateTime::parse(&end, &Rfc3339)
            .map_err(|_| CalendarPreviewError::InvalidTimestamp)?;
        if start_value >= end_value {
            return Err(CalendarPreviewError::InvalidTimeRange);
        }
        if attendees.len() > MAX_ATTENDEES {
            return Err(CalendarPreviewError::TooManyAttendees);
        }
        for attendee in &attendees {
            validate_email(attendee)?;
        }
        attendees.sort_unstable();
        let unique: HashSet<&str> = attendees.iter().map(String::as_str).collect();
        if unique.len() != attendees.len() {
            return Err(CalendarPreviewError::DuplicateAttendee);
        }
        validate_reminders(&reminders)?;
        if send_invitations && attendees.is_empty() {
            return Err(CalendarPreviewError::InvitationsWithoutAttendees);
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
        })
    }

    pub fn digest(&self) -> Result<PreviewDigest, CalendarPreviewError> {
        let value =
            serde_json::to_value(self).map_err(|_| CalendarPreviewError::SerializationFailed)?;
        let canonical =
            serde_json::to_vec(&value).map_err(|_| CalendarPreviewError::SerializationFailed)?;
        let hash: [u8; 32] = Sha256::digest(canonical).into();
        Ok(PreviewDigest::new(hash))
    }

    pub fn mutation_target(&self) -> Result<MutationTargetFingerprint, CalendarPreviewError> {
        let digest = self.digest()?;
        let combined = format!("{CALENDAR_TARGET_LABEL}:{}", hex::encode(digest.as_bytes()));
        let hash: [u8; 32] = Sha256::digest(combined.as_bytes()).into();
        Ok(MutationTargetFingerprint::new(hash))
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn start(&self) -> &str {
        &self.start
    }

    pub fn end(&self) -> &str {
        &self.end
    }

    pub fn timezone(&self) -> &str {
        &self.timezone
    }

    pub const fn calendar(&self) -> &CalendarTarget {
        &self.calendar
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn attendees(&self) -> &[String] {
        &self.attendees
    }

    pub const fn reminders(&self) -> &CalendarReminders {
        &self.reminders
    }

    pub const fn send_invitations(&self) -> bool {
        self.send_invitations
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CalendarPreviewError {
    #[error("invalid calendar event title")]
    InvalidTitle,
    #[error("invalid calendar event timestamp")]
    InvalidTimestamp,
    #[error("calendar event end must be after start")]
    InvalidTimeRange,
    #[error("invalid calendar timezone")]
    InvalidTimezone,
    #[error("invalid alternate calendar id")]
    InvalidCalendarId,
    #[error("invalid calendar event description")]
    InvalidDescription,
    #[error("invalid attendee email")]
    InvalidAttendee,
    #[error("too many calendar attendees")]
    TooManyAttendees,
    #[error("duplicate calendar attendee")]
    DuplicateAttendee,
    #[error("invalid calendar reminder")]
    InvalidReminder,
    #[error("at least one calendar reminder is required")]
    NoReminders,
    #[error("send invitations requires at least one attendee")]
    InvitationsWithoutAttendees,
    #[error("calendar preview serialization failed")]
    SerializationFailed,
}

fn validate_bounded_text(
    value: &str,
    maximum: usize,
    error: CalendarPreviewError,
) -> Result<(), CalendarPreviewError> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(error);
    }
    Ok(())
}

fn validate_timezone(value: &str) -> Result<(), CalendarPreviewError> {
    if value.trim().is_empty()
        || value.len() > MAX_TIMEZONE_BYTES
        || value
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+')))
    {
        return Err(CalendarPreviewError::InvalidTimezone);
    }
    Ok(())
}

fn validate_calendar(value: &CalendarTarget) -> Result<(), CalendarPreviewError> {
    if let CalendarTarget::Alternate { calendar_id } = value {
        validate_bounded_text(
            calendar_id,
            MAX_CALENDAR_ID_BYTES,
            CalendarPreviewError::InvalidCalendarId,
        )?;
    }
    Ok(())
}

fn validate_email(value: &str) -> Result<(), CalendarPreviewError> {
    if value.trim().is_empty()
        || value.len() > MAX_EMAIL_BYTES
        || value.chars().any(|c| c.is_ascii_control() || c == ' ')
    {
        return Err(CalendarPreviewError::InvalidAttendee);
    }
    let Some((local, domain)) = value.split_once('@') else {
        return Err(CalendarPreviewError::InvalidAttendee);
    };
    if local.is_empty()
        || domain.is_empty()
        || local.contains('@')
        || !domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(CalendarPreviewError::InvalidAttendee);
    }
    Ok(())
}

fn validate_reminders(value: &CalendarReminders) -> Result<(), CalendarPreviewError> {
    if value.push_minutes.is_none() && value.email_minutes.is_none() {
        return Err(CalendarPreviewError::NoReminders);
    }
    if value
        .push_minutes
        .into_iter()
        .chain(value.email_minutes)
        .any(|minutes| minutes > MAX_REMINDER_MINUTES)
    {
        return Err(CalendarPreviewError::InvalidReminder);
    }
    Ok(())
}

/// Calendar-specific confirmation request. The action and mutation-target
/// label are fixed by the service and cannot be selected by callers.
pub struct IssueCalendarConfirmationRequest<'a> {
    pub preview: &'a CalendarPreview,
    pub confirmation_id: ConfirmationId,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub deadline: WaitDeadline,
    pub timestamp: WorkflowTimestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum CalendarConfirmationError {
    #[error(transparent)]
    Preview(#[from] CalendarPreviewError),
    #[error(transparent)]
    Transition(#[from] domain::transition::TransitionError),
    #[error(transparent)]
    Confirmation(#[from] domain::confirmation::ConfirmationError),
}

/// Issues and consumes confirmations bound to the exact canonical Calendar
/// preview. No external provider operation is performed here.
pub struct CalendarConfirmationService;

impl CalendarConfirmationService {
    pub fn issue(
        workflow: &Workflow,
        request: IssueCalendarConfirmationRequest<'_>,
    ) -> Result<ConfirmationIssueOutcome, CalendarConfirmationError> {
        Ok(workflow.issue_confirmation(ConfirmationIssueRequest {
            confirmation_id: request.confirmation_id,
            expected_workflow_revision: workflow.revision(),
            topic: workflow.topic(),
            preview_digest: request.preview.digest()?,
            mutation_target: request.preview.mutation_target()?,
            action: ConfirmationAction::StartCalendarOrEmailAction,
            deadline: request.deadline,
            actor: request.actor,
            source_message: request.source_message,
            timestamp: request.timestamp,
        })?)
    }

    pub fn consume(
        confirmation: &ConfirmationRecord,
        workflow: &Workflow,
        authorization: &AuthorizedWorkflowAction,
        preview: &CalendarPreview,
        source: TopicMessageReference,
        confirmed_at: WorkflowTimestamp,
    ) -> Result<ConfirmationConsumption, CalendarConfirmationError> {
        Ok(confirmation.consume(
            workflow,
            authorization,
            ConfirmationConsumeRequest {
                preview_digest: preview.digest()?,
                mutation_target: preview.mutation_target()?,
                source,
                confirmed_at,
            },
        )?)
    }
}

/// Rejected calendar operation binding.
#[derive(Debug)]
pub enum CalendarFlowError {
    /// The confirmation-to-operation binding failed (revision, target, or
    /// action-kind mismatch).
    Binding(ConsumeAndPrepareError),
}

impl Display for CalendarFlowError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binding(error) => {
                write!(formatter, "calendar operation binding failed: {error}")
            }
        }
    }
}

impl std::error::Error for CalendarFlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(error) => Some(error),
        }
    }
}

impl From<ConsumeAndPrepareError> for CalendarFlowError {
    fn from(value: ConsumeAndPrepareError) -> Self {
        Self::Binding(value)
    }
}

/// A validated confirmation-to-operation binding ready for atomic persistence
/// and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCalendarEvent {
    consume_request: ConsumeAndPrepareRequest,
    retry_policy: RetryPolicy,
}

impl PreparedCalendarEvent {
    /// The atomic consume-and-prepare request for the confirmation repository.
    pub const fn consume_request(&self) -> &ConsumeAndPrepareRequest {
        &self.consume_request
    }

    /// The retry policy for the external operation executor.
    pub const fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy
    }

    /// The stable operation key reused by every physical retry attempt.
    pub const fn operation_key(&self) -> &IdempotencyKey {
        self.consume_request.operation_key()
    }
}

/// Stateless orchestration for confirmed Calendar event creation.
///
/// Binds a consumed [`ConfirmationConsumption`] to the [`IdempotencyKey`] the
/// external operation executor will reuse for every retry attempt.  The key
/// binds:
/// - the workflow id and **resulting** revision (after consumption),
/// - [`OperationKind::GoogleWrite`], and
/// - the confirmation's mutation-target fingerprint as the
///   [`OperationTargetFingerprint`].
///
/// [`ConsumeAndPrepareRequest::new`] then validates that the confirmation
/// action pairs with `GoogleWrite`, that the target matches, and that the
/// revisions agree — all before any I/O.
pub struct CalendarOperationService;

impl CalendarOperationService {
    /// Build the operation binding from a freshly consumed confirmation.
    ///
    /// The caller must have already obtained `consumption` from
    /// [`crate::resumable_confirmation::ResumableConfirmationService::consume_confirmation`],
    /// which rejects stale (already-consumed) confirmations at the domain
    /// layer.  This method then constructs and validates the durable operation
    /// key.
    pub fn prepare(
        consumption: ConfirmationConsumption,
        retry_policy: RetryPolicy,
    ) -> Result<PreparedCalendarEvent, CalendarFlowError> {
        let transition = consumption.transition();
        let resulting_revision: WorkflowRevision = transition.workflow.revision();
        let mutation_target = consumption.confirmation().mutation_target();
        let target: OperationTargetFingerprint =
            OperationTargetFingerprint::new(*mutation_target.as_bytes());
        let operation_key = IdempotencyKey::new(
            transition.workflow.id().clone(),
            resulting_revision,
            OperationKind::GoogleWrite,
            target,
        );
        let consume_request = ConsumeAndPrepareRequest::new(consumption, operation_key)?;
        Ok(PreparedCalendarEvent {
            consume_request,
            retry_policy,
        })
    }
}
