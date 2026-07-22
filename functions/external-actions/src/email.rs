#![deny(unsafe_code)]

//! Email action handler (Task 37A).
//!
//! Validates the proof-to-request binding, reconstructs an
//! [`EmailMessageRequest`] from the canonical [`EmailPreview`], calls an
//! injected adapter runner, and maps [`ProviderOutcome`] to a serializable
//! [`ExternalActionResultDto`].

use serde::Deserialize;

use application::email::EmailPreview;
use application::external_operation::ProviderOutcome;
use domain::confirmation::{MutationTargetFingerprint, PreviewDigest};
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;
use email::message::{ConfirmedEmailProof, EmailMessageRequest, SendError};

use crate::EVENT_SCHEMA_VERSION;
use crate::google::ExternalActionResultDto;

/// Versioned event for sending an email.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailActionEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub recipients: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    #[serde(rename = "attachQuotationPdf")]
    pub attach_quotation_pdf: bool,
    #[serde(rename = "includeSevenDayLink")]
    pub include_seven_day_link: bool,
    #[serde(rename = "linkExpiryDays")]
    pub link_expiry_days: u16,
    pub owner: i64,
    #[serde(rename = "mutationTargetHex")]
    pub mutation_target_hex: String,
    #[serde(rename = "previewDigestHex")]
    pub preview_digest_hex: String,
    #[serde(rename = "workflowRevision")]
    pub workflow_revision: u64,
}

/// Abstraction over the email adapter so the handler is unit-testable
/// without SMTP credentials.
#[allow(async_fn_in_trait)]
pub trait EmailActionRunner {
    async fn run(
        &self,
        proof: &ConfirmedEmailProof,
        request: &EmailMessageRequest,
    ) -> Result<ProviderOutcome, SendError>;
}

/// Pure preprocessing: validate the event, rebuild proof + request from the
/// canonical preview, and delegate to the injected [`EmailActionRunner`].
pub async fn process_email_action<R: EmailActionRunner>(
    event: EmailActionEvent,
    runner: &R,
) -> Result<ExternalActionResultDto, EmailProcessError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(EmailProcessError::SchemaVersionMismatch);
    }

    let owner = ParticipantId::new(event.owner).map_err(|_| EmailProcessError::InvalidOwner)?;
    let mutation_target = rebuild_mutation_target(&event.mutation_target_hex)?;
    let preview_digest = rebuild_preview_digest(&event.preview_digest_hex)?;
    let workflow_revision = WorkflowRevision::new(event.workflow_revision);
    let proof = ConfirmedEmailProof::new(owner, mutation_target, preview_digest, workflow_revision);

    let preview = EmailPreview::new(
        event.recipients,
        event.cc,
        event.bcc,
        event.subject,
        event.body,
        event.attach_quotation_pdf,
        event.include_seven_day_link,
        event.link_expiry_days,
    )
    .map_err(|_| EmailProcessError::InvalidPreview)?;

    let request = EmailMessageRequest::from_preview(&preview)
        .map_err(|_| EmailProcessError::InvalidRequest)?;

    if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
        return Err(EmailProcessError::TargetMismatch);
    }

    let outcome = runner
        .run(&proof, &request)
        .await
        .map_err(EmailProcessError::Send)?;

    Ok(ExternalActionResultDto::from(outcome))
}

// ── Helpers ───────────────────────────────────────────────────────────

fn rebuild_mutation_target(hex: &str) -> Result<MutationTargetFingerprint, EmailProcessError> {
    let bytes = hex::decode(hex).map_err(|_| EmailProcessError::InvalidMutationTarget)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| EmailProcessError::InvalidMutationTarget)?;
    Ok(MutationTargetFingerprint::new(array))
}

fn rebuild_preview_digest(hex: &str) -> Result<PreviewDigest, EmailProcessError> {
    let bytes = hex::decode(hex).map_err(|_| EmailProcessError::InvalidPreviewDigest)?;
    let array = <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| EmailProcessError::InvalidPreviewDigest)?;
    Ok(PreviewDigest::new(array))
}

// ── Errors ────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum EmailProcessError {
    SchemaVersionMismatch,
    InvalidOwner,
    InvalidMutationTarget,
    InvalidPreviewDigest,
    InvalidPreview,
    InvalidRequest,
    TargetMismatch,
    Send(SendError),
}

impl std::fmt::Display for EmailProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => f.write_str("email event: schema version mismatch"),
            Self::InvalidOwner => f.write_str("email event: invalid owner"),
            Self::InvalidMutationTarget => f.write_str("email event: invalid mutation target hex"),
            Self::InvalidPreviewDigest => f.write_str("email event: invalid preview digest hex"),
            Self::InvalidPreview => f.write_str("email event: invalid preview"),
            Self::InvalidRequest => f.write_str("email event: invalid request"),
            Self::TargetMismatch => f.write_str("email event: target mismatch"),
            Self::Send(e) => write!(f, "email: {e}"),
        }
    }
}

impl std::error::Error for EmailProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Send(e) => Some(e),
            _ => None,
        }
    }
}
