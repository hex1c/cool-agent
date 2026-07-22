#![deny(unsafe_code)]

//! Google Calendar action handler (Task 37A).
//!
//! Validates the proof-to-request binding, reconstructs a
//! [`CalendarEventRequest`] from the canonical [`CalendarPreview`], calls an
//! injected adapter runner, and maps [`ProviderOutcome`] to a serializable
//! [`ExternalActionResultDto`].

use serde::Deserialize;

use application::calendar::{CalendarPreview, CalendarReminders, CalendarTarget};
use application::external_operation::ProviderOutcome;
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;
use google::calendar::{CalendarEventRequest, ConfirmedCalendarProof, CreateEventError};

use crate::EVENT_SCHEMA_VERSION;
use crate::google::ExternalActionResultDto;

/// Versioned event for creating a Google Calendar event.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarActionEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub timezone: String,
    #[serde(rename = "calendarId")]
    pub calendar_id: Option<String>,
    pub description: Option<String>,
    pub attendees: Vec<String>,
    #[serde(rename = "remindersPushMinutes")]
    pub reminders_push_minutes: Option<u16>,
    #[serde(rename = "remindersEmailMinutes")]
    pub reminders_email_minutes: Option<u16>,
    #[serde(rename = "sendInvitations")]
    pub send_invitations: bool,
    pub owner: i64,
    #[serde(rename = "mutationTargetHex")]
    pub mutation_target_hex: String,
    #[serde(rename = "previewDigestHex")]
    pub preview_digest_hex: String,
    #[serde(rename = "workflowRevision")]
    pub workflow_revision: u64,
}

/// Abstraction over the Google Calendar adapter so the handler is
/// unit-testable without credentials.
#[allow(async_fn_in_trait)]
pub trait CalendarActionRunner {
    async fn run(
        &self,
        proof: &ConfirmedCalendarProof,
        request: &CalendarEventRequest,
    ) -> Result<ProviderOutcome, CreateEventError>;
}

/// Pure preprocessing: validate the event, rebuild proof + request from the
/// canonical preview, and delegate to the injected [`CalendarActionRunner`].
pub async fn process_calendar_action<R: CalendarActionRunner>(
    event: CalendarActionEvent,
    runner: &R,
) -> Result<ExternalActionResultDto, CalendarProcessError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(CalendarProcessError::SchemaVersionMismatch);
    }

    let owner = ParticipantId::new(event.owner).map_err(|_| CalendarProcessError::InvalidOwner)?;
    let mutation_target = rebuild_mutation_target(&event.mutation_target_hex)?;
    let preview_digest = rebuild_preview_digest(&event.preview_digest_hex)?;
    let workflow_revision = WorkflowRevision::new(event.workflow_revision);
    let proof =
        ConfirmedCalendarProof::new(owner, mutation_target, preview_digest, workflow_revision);

    let calendar = match event.calendar_id {
        Some(id) => CalendarTarget::Alternate { calendar_id: id },
        None => CalendarTarget::Primary,
    };
    let reminders = CalendarReminders {
        push_minutes: event.reminders_push_minutes,
        email_minutes: event.reminders_email_minutes,
    };
    let preview = CalendarPreview::new(
        event.title,
        event.start,
        event.end,
        event.timezone,
        calendar,
        event.description,
        event.attendees,
        reminders,
        event.send_invitations,
    )
    .map_err(|_| CalendarProcessError::InvalidPreview)?;

    let request = CalendarEventRequest::from_preview(&preview)
        .map_err(|_| CalendarProcessError::InvalidRequest)?;

    if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
        return Err(CalendarProcessError::TargetMismatch);
    }

    let outcome = runner
        .run(&proof, &request)
        .await
        .map_err(CalendarProcessError::Calendar)?;

    Ok(ExternalActionResultDto::from(outcome))
}

// ── Helpers ───────────────────────────────────────────────────────────

fn rebuild_mutation_target(hex: &str) -> Result<MutationTargetFingerprint, CalendarProcessError> {
    let bytes = hex::decode(hex).map_err(|_| CalendarProcessError::InvalidMutationTarget)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| CalendarProcessError::InvalidMutationTarget)?;
    Ok(MutationTargetFingerprint::new(array))
}

fn rebuild_preview_digest(hex: &str) -> Result<PreviewDigest, CalendarProcessError> {
    let bytes = hex::decode(hex).map_err(|_| CalendarProcessError::InvalidPreviewDigest)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| CalendarProcessError::InvalidPreviewDigest)?;
    Ok(PreviewDigest::new(array))
}

// ── Errors ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CalendarProcessError {
    SchemaVersionMismatch,
    InvalidOwner,
    InvalidMutationTarget,
    InvalidPreviewDigest,
    InvalidPreview,
    InvalidRequest,
    TargetMismatch,
    Calendar(CreateEventError),
}

impl std::fmt::Display for CalendarProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => f.write_str("calendar event: schema version mismatch"),
            Self::InvalidOwner => f.write_str("calendar event: invalid owner"),
            Self::InvalidMutationTarget => {
                f.write_str("calendar event: invalid mutation target hex")
            }
            Self::InvalidPreviewDigest => f.write_str("calendar event: invalid preview digest hex"),
            Self::InvalidPreview => f.write_str("calendar event: invalid preview"),
            Self::InvalidRequest => f.write_str("calendar event: invalid request"),
            Self::TargetMismatch => f.write_str("calendar event: target mismatch"),
            Self::Calendar(e) => write!(f, "calendar: {e}"),
        }
    }
}

impl std::error::Error for CalendarProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Calendar(e) => Some(e),
            _ => None,
        }
    }
}
