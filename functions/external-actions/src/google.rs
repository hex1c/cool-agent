#![deny(unsafe_code)]

//! Google Workspace action handlers (Task 37A).
//!
//! Two event variants covering new-file creation and existing-file mutation.
//! Each handler validates the proof-to-request binding, calls an injected
//! adapter runner, and maps [`ProviderOutcome`] to a serializable
//! [`ExternalActionResultDto`].

use serde::{Deserialize, Serialize};

use crate::EVENT_SCHEMA_VERSION;
use application::external_operation::ProviderOutcome;
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;
use google::drive::{FolderId, ResolvedDestination, SharedDriveId};
use google::existing_files::{
    ConfirmedMutationProof, FieldUpdate, FileResourceId, MutationPayload, TabOrSection,
};
use google::mutations::MutationError;
use google::sheets_docs::CreateFileError;
use google::sheets_docs::{ConfirmedCreateProof, FileKind, NewFileError, NewFileRequest};

// ── Shared result DTO ─────────────────────────────────────────────────

/// Typed external-action outcome. Raw provider payloads and secrets are never
/// serialized — only stable labels and opaque resource ids.
#[derive(Debug, Clone, Serialize)]
pub struct ExternalActionResultDto {
    #[serde(rename = "outcome")]
    pub outcome: &'static str,
    #[serde(rename = "resourceId")]
    pub resource_id: Option<String>,
    #[serde(rename = "failureCode")]
    pub failure_code: Option<String>,
    #[serde(rename = "failureSummary")]
    pub failure_summary: Option<String>,
}

impl From<ProviderOutcome> for ExternalActionResultDto {
    fn from(value: ProviderOutcome) -> Self {
        match value {
            ProviderOutcome::Accepted { resource_id } => Self {
                outcome: "accepted",
                resource_id: resource_id.map(|id| id.as_str().to_owned()),
                failure_code: None,
                failure_summary: None,
            },
            ProviderOutcome::RetryableFailure(f) => Self {
                outcome: "retryable_failure",
                resource_id: None,
                failure_code: Some(f.code().as_str().to_owned()),
                failure_summary: Some(f.summary().as_str().to_owned()),
            },
            ProviderOutcome::TerminalFailure(f) => Self {
                outcome: "terminal_failure",
                resource_id: None,
                failure_code: Some(f.code().as_str().to_owned()),
                failure_summary: Some(f.summary().as_str().to_owned()),
            },
            ProviderOutcome::Ambiguous(f) => Self {
                outcome: "ambiguous",
                resource_id: None,
                failure_code: Some(f.code().as_str().to_owned()),
                failure_summary: Some(f.summary().as_str().to_owned()),
            },
        }
    }
}

// ── Google Create event ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileKindDto {
    #[serde(rename = "kind")]
    pub kind: String,
}

impl TryFrom<FileKindDto> for FileKind {
    type Error = GoogleCreateProcessError;

    fn try_from(value: FileKindDto) -> Result<Self, Self::Error> {
        match value.kind.as_str() {
            "sheet" => Ok(FileKind::Sheet),
            "doc" => Ok(FileKind::Doc),
            _ => Err(GoogleCreateProcessError::InvalidFileKind),
        }
    }
}

/// Versioned event for creating a new Google Workspace file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCreateEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub kind: FileKindDto,
    pub title: String,
    #[serde(rename = "destinationFolderId")]
    pub destination_folder_id: Option<String>,
    #[serde(rename = "destinationSharedDriveId")]
    pub destination_shared_drive_id: Option<String>,
    pub owner: i64,
    #[serde(rename = "mutationTargetHex")]
    pub mutation_target_hex: String,
    #[serde(rename = "previewDigestHex")]
    pub preview_digest_hex: String,
    #[serde(rename = "workflowRevision")]
    pub workflow_revision: u64,
}

/// Abstraction over the Google file-creation adapter so the handler is
/// unit-testable without credentials.
#[allow(async_fn_in_trait)]
pub trait GoogleCreateRunner {
    async fn run(
        &self,
        proof: &ConfirmedCreateProof,
        request: &NewFileRequest,
    ) -> Result<ProviderOutcome, CreateFileError>;
}

/// Pure preprocessing: validate the event, rebuild proof + request, call
/// service validate, and delegate to the injected [`GoogleCreateRunner`].
pub async fn process_google_create<R: GoogleCreateRunner>(
    event: GoogleCreateEvent,
    runner: &R,
) -> Result<ExternalActionResultDto, GoogleCreateProcessError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(GoogleCreateProcessError::SchemaVersionMismatch);
    }

    let owner =
        ParticipantId::new(event.owner).map_err(|_| GoogleCreateProcessError::InvalidOwner)?;
    let mutation_target = rebuild_mutation_target(&event.mutation_target_hex)?;
    let preview_digest = rebuild_preview_digest(&event.preview_digest_hex)?;
    let workflow_revision = WorkflowRevision::new(event.workflow_revision);
    let proof =
        ConfirmedCreateProof::new(owner, mutation_target, preview_digest, workflow_revision);

    let kind: FileKind = event.kind.try_into()?;
    let folder_id = event
        .destination_folder_id
        .map(|id| FolderId::new(id).map_err(|_| GoogleCreateProcessError::InvalidDestination))
        .transpose()?;
    let shared_drive = event
        .destination_shared_drive_id
        .map(|id| SharedDriveId::new(id).map_err(|_| GoogleCreateProcessError::InvalidDestination))
        .transpose()?;
    let destination = ResolvedDestination::new(folder_id, shared_drive);
    let target_fingerprint = OperationTargetFingerprint::new(*proof.mutation_target().as_bytes());
    let request = NewFileRequest::new(kind, event.title, destination, target_fingerprint)
        .map_err(GoogleCreateProcessError::from)?;

    if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
        return Err(GoogleCreateProcessError::TargetMismatch);
    }

    let outcome = runner
        .run(&proof, &request)
        .await
        .map_err(GoogleCreateProcessError::Create)?;

    Ok(ExternalActionResultDto::from(outcome))
}

// ── Google Mutation event ─────────────────────────────────────────────

/// Serializable field update from the event.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldUpdateDto {
    pub field: String,
    #[serde(rename = "resultingValue")]
    pub resulting_value: String,
}

/// Versioned event for mutating an existing Google Workspace file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleMutationEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    #[serde(rename = "fileId")]
    pub file_id: String,
    #[serde(rename = "fileKind")]
    pub file_kind: FileKindDto,
    pub section: String,
    pub updates: Vec<FieldUpdateDto>,
    pub owner: i64,
    #[serde(rename = "actor")]
    pub actor: i64,
    #[serde(rename = "mutationTargetHex")]
    pub mutation_target_hex: String,
    #[serde(rename = "previewDigestHex")]
    pub preview_digest_hex: String,
    #[serde(rename = "workflowRevision")]
    pub workflow_revision: u64,
}

/// Abstraction over the Google mutation adapter so the handler is
/// unit-testable without credentials.
#[allow(async_fn_in_trait)]
pub trait GoogleMutationRunner {
    async fn run(
        &self,
        proof: &ConfirmedMutationProof,
        payload: &MutationPayload,
    ) -> Result<ProviderOutcome, MutationError>;
}

/// Pure preprocessing: validate the event, rebuild proof + payload, call
/// service validate, and delegate to the injected [`GoogleMutationRunner`].
pub async fn process_google_mutation<R: GoogleMutationRunner>(
    event: GoogleMutationEvent,
    runner: &R,
) -> Result<ExternalActionResultDto, GoogleMutationProcessError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(GoogleMutationProcessError::SchemaVersionMismatch);
    }

    let owner =
        ParticipantId::new(event.owner).map_err(|_| GoogleMutationProcessError::InvalidOwner)?;
    let mutation_target = rebuild_mutation_target(&event.mutation_target_hex)?;
    let preview_digest = rebuild_preview_digest(&event.preview_digest_hex)?;
    let workflow_revision = WorkflowRevision::new(event.workflow_revision);
    let proof =
        ConfirmedMutationProof::new(owner, mutation_target, preview_digest, workflow_revision);

    let file = FileResourceId::new(event.file_id)
        .map_err(|_| GoogleMutationProcessError::InvalidFileId)?;
    let _file_kind: FileKind = event
        .file_kind
        .try_into()
        .map_err(|_e: GoogleCreateProcessError| GoogleMutationProcessError::InvalidFileKind)?;
    let section =
        TabOrSection::new(event.section).map_err(|_| GoogleMutationProcessError::InvalidSection)?;
    let updates: Vec<FieldUpdate> = event
        .updates
        .into_iter()
        .map(|u| FieldUpdate::new(u.field, u.resulting_value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| GoogleMutationProcessError::InvalidFieldUpdate)?;
    let target_fingerprint = OperationTargetFingerprint::new(*proof.mutation_target().as_bytes());
    let payload = MutationPayload::new(file, section, updates, target_fingerprint)
        .map_err(|_| GoogleMutationProcessError::InvalidPayload)?;

    if proof.mutation_target().as_bytes() != payload.target_fingerprint().as_bytes() {
        return Err(GoogleMutationProcessError::TargetMismatch);
    }

    let outcome = runner
        .run(&proof, &payload)
        .await
        .map_err(GoogleMutationProcessError::Mutation)?;

    Ok(ExternalActionResultDto::from(outcome))
}

// ── Helper functions ──────────────────────────────────────────────────

fn rebuild_mutation_target(
    hex: &str,
) -> Result<MutationTargetFingerprint, GoogleCreateProcessError> {
    let bytes = hex::decode(hex).map_err(|_| GoogleCreateProcessError::InvalidMutationTarget)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| GoogleCreateProcessError::InvalidMutationTarget)?;
    Ok(MutationTargetFingerprint::new(array))
}

fn rebuild_preview_digest(hex: &str) -> Result<PreviewDigest, GoogleCreateProcessError> {
    let bytes = hex::decode(hex).map_err(|_| GoogleCreateProcessError::InvalidPreviewDigest)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| GoogleCreateProcessError::InvalidPreviewDigest)?;
    Ok(PreviewDigest::new(array))
}

// ── Error types ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum GoogleCreateProcessError {
    SchemaVersionMismatch,
    InvalidOwner,
    InvalidMutationTarget,
    InvalidPreviewDigest,
    InvalidDestination,
    InvalidFileKind,
    InvalidTitle,
    TargetMismatch,
    Unauthorized,
    Create(CreateFileError),
}

impl std::fmt::Display for GoogleCreateProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => {
                f.write_str("google create event: schema version mismatch")
            }
            Self::InvalidOwner => f.write_str("google create event: invalid owner"),
            Self::InvalidMutationTarget => {
                f.write_str("google create event: invalid mutation target hex")
            }
            Self::InvalidPreviewDigest => {
                f.write_str("google create event: invalid preview digest hex")
            }
            Self::InvalidDestination => f.write_str("google create event: invalid destination"),
            Self::InvalidFileKind => f.write_str("google create event: invalid file kind"),
            Self::InvalidTitle => f.write_str("google create event: invalid title"),
            Self::TargetMismatch => f.write_str("google create event: target mismatch"),
            Self::Unauthorized => f.write_str("google create event: unauthorized"),
            Self::Create(e) => write!(f, "google create: {e}"),
        }
    }
}

impl std::error::Error for GoogleCreateProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Create(e) => Some(e),
            _ => None,
        }
    }
}

impl From<NewFileError> for GoogleCreateProcessError {
    fn from(value: NewFileError) -> Self {
        match value {
            NewFileError::InvalidTitle => Self::InvalidTitle,
            NewFileError::Unauthorized => Self::Unauthorized,
            NewFileError::TargetMismatch => Self::TargetMismatch,
        }
    }
}

impl From<GoogleCreateProcessError> for GoogleMutationProcessError {
    fn from(value: GoogleCreateProcessError) -> Self {
        match value {
            GoogleCreateProcessError::SchemaVersionMismatch => Self::SchemaVersionMismatch,
            GoogleCreateProcessError::InvalidOwner => Self::InvalidOwner,
            GoogleCreateProcessError::InvalidMutationTarget => Self::InvalidMutationTarget,
            GoogleCreateProcessError::InvalidPreviewDigest => Self::InvalidPreviewDigest,
            GoogleCreateProcessError::InvalidDestination => Self::InvalidFileId,
            GoogleCreateProcessError::InvalidFileKind => Self::InvalidFileKind,
            GoogleCreateProcessError::InvalidTitle => Self::InvalidPayload,
            GoogleCreateProcessError::TargetMismatch => Self::TargetMismatch,
            GoogleCreateProcessError::Unauthorized => Self::Unauthorized,
            GoogleCreateProcessError::Create(_) => Self::Sanitization,
        }
    }
}

#[derive(Debug)]
pub enum GoogleMutationProcessError {
    SchemaVersionMismatch,
    InvalidOwner,
    InvalidMutationTarget,
    InvalidPreviewDigest,
    InvalidFileId,
    InvalidFileKind,
    InvalidSection,
    InvalidFieldUpdate,
    InvalidPayload,
    TargetMismatch,
    Unauthorized,
    Sanitization,
    Mutation(MutationError),
}

impl std::fmt::Display for GoogleMutationProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => {
                f.write_str("google mutation event: schema version mismatch")
            }
            Self::InvalidOwner => f.write_str("google mutation event: invalid owner"),
            Self::InvalidMutationTarget => {
                f.write_str("google mutation event: invalid mutation target hex")
            }
            Self::InvalidPreviewDigest => {
                f.write_str("google mutation event: invalid preview digest hex")
            }
            Self::InvalidFileId => f.write_str("google mutation event: invalid file id"),
            Self::InvalidFileKind => f.write_str("google mutation event: invalid file kind"),
            Self::InvalidSection => f.write_str("google mutation event: invalid section"),
            Self::InvalidFieldUpdate => f.write_str("google mutation event: invalid field update"),
            Self::InvalidPayload => f.write_str("google mutation event: invalid payload"),
            Self::TargetMismatch => f.write_str("google mutation event: target mismatch"),
            Self::Unauthorized => f.write_str("google mutation event: unauthorized"),
            Self::Sanitization => f.write_str("google mutation event: sanitization error"),
            Self::Mutation(e) => write!(f, "google mutation: {e}"),
        }
    }
}

impl std::error::Error for GoogleMutationProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mutation(e) => Some(e),
            _ => None,
        }
    }
}
