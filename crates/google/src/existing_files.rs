pub use crate::sheets_docs::FileKind;

use domain::confirmation::{
    ConfirmationAction, ConfirmationRecord, ConfirmationStatus, MutationTargetFingerprint,
    PreviewDigest,
};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;

/// A validated Google file resource identifier (opaque alphanumeric id).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileResourceId(String);

impl FileResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ExistingFileError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 {
            return Err(ExistingFileError::InvalidFileResourceId);
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ExistingFileError::InvalidFileResourceId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifies a tab name within a Sheet or a section within a Doc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TabOrSection(String);

impl TabOrSection {
    pub fn new(value: impl Into<String>) -> Result<Self, ExistingFileError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(ExistingFileError::InvalidTabOrSection);
        }
        if value.bytes().any(|b| b.is_ascii_control()) {
            return Err(ExistingFileError::InvalidTabOrSection);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single field update specifying the field name and its resulting value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldUpdate {
    field: String,
    resulting_value: String,
}

impl FieldUpdate {
    pub fn new(
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, ExistingFileError> {
        let field = field.into();
        let value = value.into();
        if field.is_empty() || field.len() > 256 || value.is_empty() || value.len() > 256 {
            return Err(ExistingFileError::InvalidFieldUpdate);
        }
        if field.bytes().any(|b| b.is_ascii_control())
            || value.bytes().any(|b| b.is_ascii_control())
        {
            return Err(ExistingFileError::InvalidFieldUpdate);
        }
        Ok(Self {
            field,
            resulting_value: value,
        })
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn resulting_value(&self) -> &str {
        &self.resulting_value
    }
}

/// Preview of an existing-file mutation identifying the file, tab/section,
/// fields, resulting values, owner, and actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingFilePreview {
    file: FileResourceId,
    kind: FileKind,
    section: TabOrSection,
    updates: Vec<FieldUpdate>,
    owner: ParticipantId,
    actor: ParticipantId,
}

impl ExistingFilePreview {
    pub fn new(
        file: FileResourceId,
        kind: FileKind,
        section: TabOrSection,
        updates: Vec<FieldUpdate>,
        owner: ParticipantId,
        actor: ParticipantId,
    ) -> Result<Self, ExistingFileError> {
        if updates.is_empty() {
            return Err(ExistingFileError::EmptyUpdates);
        }
        Ok(Self {
            file,
            kind,
            section,
            updates,
            owner,
            actor,
        })
    }

    pub fn file(&self) -> &FileResourceId {
        &self.file
    }

    pub fn kind(&self) -> FileKind {
        self.kind
    }

    pub fn section(&self) -> &TabOrSection {
        &self.section
    }

    pub fn updates(&self) -> &[FieldUpdate] {
        &self.updates
    }

    pub fn owner(&self) -> ParticipantId {
        self.owner
    }

    pub fn actor(&self) -> ParticipantId {
        self.actor
    }
}

/// The exact mutation payload bound to a target fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationPayload {
    file: FileResourceId,
    section: TabOrSection,
    updates: Vec<FieldUpdate>,
    target_fingerprint: OperationTargetFingerprint,
}

impl MutationPayload {
    pub fn new(
        file: FileResourceId,
        section: TabOrSection,
        updates: Vec<FieldUpdate>,
        target_fingerprint: OperationTargetFingerprint,
    ) -> Result<Self, ExistingFileError> {
        if updates.is_empty() {
            return Err(ExistingFileError::EmptyUpdates);
        }
        Ok(Self {
            file,
            section,
            updates,
            target_fingerprint,
        })
    }

    pub fn file(&self) -> &FileResourceId {
        &self.file
    }

    pub fn section(&self) -> &TabOrSection {
        &self.section
    }

    pub fn updates(&self) -> &[FieldUpdate] {
        &self.updates
    }

    pub fn target_fingerprint(&self) -> OperationTargetFingerprint {
        self.target_fingerprint
    }
}

/// Proof extracted from a consumed confirmation that authorizes a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedMutationProof {
    owner: ParticipantId,
    mutation_target: MutationTargetFingerprint,
    preview_digest: PreviewDigest,
    workflow_revision: WorkflowRevision,
}

impl ConfirmedMutationProof {
    pub fn from_consumed(record: &ConfirmationRecord) -> Result<Self, ExistingFileError> {
        if record.status() != ConfirmationStatus::Consumed {
            return Err(ExistingFileError::Unauthorized);
        }
        if record.action() != ConfirmationAction::StartSheetOrDocWrite {
            return Err(ExistingFileError::Unauthorized);
        }
        Ok(Self {
            owner: record.owner(),
            mutation_target: record.mutation_target(),
            preview_digest: record.preview_digest(),
            workflow_revision: record.workflow_revision(),
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

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExistingFileError {
    #[error("invalid file resource id")]
    InvalidFileResourceId,
    #[error("invalid tab or section")]
    InvalidTabOrSection,
    #[error("invalid field update")]
    InvalidFieldUpdate,
    #[error("no field updates")]
    EmptyUpdates,
    #[error("confirmation does not authorize this mutation")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}
