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
use crate::drive::ResolvedDestination;

/// The kind of Google Workspace file to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Sheet,
    Doc,
}

/// A request to create a new file, validated against a confirmation proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFileRequest {
    kind: FileKind,
    title: String,
    destination: ResolvedDestination,
    target_fingerprint: OperationTargetFingerprint,
}

impl NewFileRequest {
    pub fn new(
        kind: FileKind,
        title: String,
        destination: ResolvedDestination,
        target_fingerprint: OperationTargetFingerprint,
    ) -> Result<Self, NewFileError> {
        if title.is_empty() || title.len() > 256 {
            return Err(NewFileError::InvalidTitle);
        }
        if title.chars().any(char::is_control) {
            return Err(NewFileError::InvalidTitle);
        }
        Ok(Self {
            kind,
            title,
            destination,
            target_fingerprint,
        })
    }

    pub fn target_fingerprint(&self) -> OperationTargetFingerprint {
        self.target_fingerprint
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum NewFileError {
    #[error("invalid title")]
    InvalidTitle,
    #[error("confirmation does not authorize this creation")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
}

/// A proof extracted from a consumed confirmation that authorizes a file creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedCreateProof {
    owner: ParticipantId,
    mutation_target: MutationTargetFingerprint,
    preview_digest: PreviewDigest,
    workflow_revision: WorkflowRevision,
}

impl ConfirmedCreateProof {
    pub fn from_consumed(record: &ConfirmationRecord) -> Result<Self, NewFileError> {
        if record.status() != ConfirmationStatus::Consumed {
            return Err(NewFileError::Unauthorized);
        }
        if record.action() != ConfirmationAction::StartSheetOrDocWrite {
            return Err(NewFileError::Unauthorized);
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

impl ConfirmedCreateProof {
    pub const fn new(
        owner: ParticipantId,
        mutation_target: MutationTargetFingerprint,
        preview_digest: PreviewDigest,
        workflow_revision: WorkflowRevision,
    ) -> Self {
        Self {
            owner,
            mutation_target,
            preview_digest,
            workflow_revision,
        }
    }
}

#[allow(async_fn_in_trait)]
pub trait GoogleCreateClient {
    type Error: Display + fmt::Debug;

    async fn create_file(
        &self,
        token: &GoogleAccessToken,
        kind: FileKind,
        title: &str,
        destination: &ResolvedDestination,
    ) -> Result<ExternalResourceId, Self::Error>;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CreateFileError {
    #[error("confirmation does not authorize this creation")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

pub struct GoogleCreateService<Client: GoogleCreateClient> {
    client: Client,
}

impl<Client: GoogleCreateClient> GoogleCreateService<Client> {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub fn validate(
        proof: &ConfirmedCreateProof,
        request: &NewFileRequest,
    ) -> Result<(), NewFileError> {
        if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
            return Err(NewFileError::TargetMismatch);
        }
        Ok(())
    }

    pub async fn create_file(
        &self,
        token: &GoogleAccessToken,
        proof: &ConfirmedCreateProof,
        request: &NewFileRequest,
    ) -> Result<ProviderOutcome, CreateFileError> {
        Self::validate(proof, request).map_err(|e| match e {
            NewFileError::Unauthorized => CreateFileError::Unauthorized,
            NewFileError::TargetMismatch => CreateFileError::TargetMismatch,
            NewFileError::InvalidTitle => CreateFileError::Sanitization,
        })?;

        match self
            .client
            .create_file(token, request.kind, &request.title, &request.destination)
            .await
        {
            Ok(resource_id) => Ok(ProviderOutcome::Accepted {
                resource_id: Some(resource_id),
            }),
            Err(_) => {
                let code = FailureCode::new("google_create_failed")
                    .map_err(|_| CreateFileError::Sanitization)?;
                let summary = SanitizedSummary::new("google create attempt failed")
                    .map_err(|_| CreateFileError::Sanitization)?;
                Ok(ProviderOutcome::RetryableFailure(OperationFailure::new(
                    code, summary,
                )))
            }
        }
    }
}
