//! Bounded value types for email messages (Task 37).
//!
//! Validates and carries the exact payload shown in the confirmation preview.
//! Every type rejects construction from untrusted data that doesn't pass the
//! canonical validation rules.

use application::email::EmailPreview;
use application::external_operation::{FailureCode, OperationFailure, SanitizedSummary};
use domain::confirmation::{
    ConfirmationAction, ConfirmationRecord, ConfirmationStatus, MutationTargetFingerprint,
    PreviewDigest,
};
use domain::idempotency::OperationTargetFingerprint;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;

// ── bounded value types ───────────────────────────────────────────────

const MAX_EMAIL_BYTES: usize = 320;
const MAX_SUBJECT_BYTES: usize = 256;
const MAX_BODY_BYTES: usize = 16_384;
const MAX_RECIPIENTS: usize = 50;
const MAX_CC: usize = 20;
const MAX_BCC: usize = 20;

/// A validated email address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailAddress(String);

impl EmailAddress {
    pub fn new(value: impl Into<String>) -> Result<Self, EmailError> {
        let value = value.into();
        if value.trim().is_empty()
            || value.len() > MAX_EMAIL_BYTES
            || value.chars().any(|c| c.is_ascii_control() || c == ' ')
        {
            return Err(EmailError::InvalidAddress);
        }
        let Some((local, domain)) = value.split_once('@') else {
            return Err(EmailError::InvalidAddress);
        };
        if local.is_empty()
            || domain.is_empty()
            || local.contains('@')
            || !domain
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return Err(EmailError::InvalidAddress);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated, non-empty email subject.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EmailSubject(String);

impl EmailSubject {
    pub fn new(value: impl Into<String>) -> Result<Self, EmailError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_SUBJECT_BYTES {
            return Err(EmailError::InvalidSubject);
        }
        if value.chars().any(char::is_control) {
            return Err(EmailError::InvalidSubject);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated, non-empty email body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailBody(String);

impl EmailBody {
    pub fn new(value: impl Into<String>) -> Result<Self, EmailError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > MAX_BODY_BYTES {
            return Err(EmailError::InvalidBody);
        }
        if value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\r')
        {
            return Err(EmailError::InvalidBody);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ── request + proof ───────────────────────────────────────────────────

/// A request to send one email, validated against a confirmation proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessageRequest {
    recipients: Vec<EmailAddress>,
    cc: Vec<EmailAddress>,
    bcc: Vec<EmailAddress>,
    subject: EmailSubject,
    body: EmailBody,
    attach_quotation_pdf: bool,
    include_seven_day_link: bool,
    link_expiry_days: u16,
    target_fingerprint: OperationTargetFingerprint,
}

impl EmailMessageRequest {
    /// Build the provider request only from the exact canonical preview shown
    /// to participants. Callers cannot supply an independent fingerprint.
    pub fn from_preview(preview: &EmailPreview) -> Result<Self, EmailError> {
        if preview.recipients().len() > MAX_RECIPIENTS {
            return Err(EmailError::TooManyRecipients);
        }
        if preview.cc().len() > MAX_CC {
            return Err(EmailError::TooManyCc);
        }
        if preview.bcc().len() > MAX_BCC {
            return Err(EmailError::TooManyBcc);
        }
        let recipients = preview
            .recipients()
            .iter()
            .map(|e| EmailAddress::new(e.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let cc = preview
            .cc()
            .iter()
            .map(|e| EmailAddress::new(e.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let bcc = preview
            .bcc()
            .iter()
            .map(|e| EmailAddress::new(e.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        let mutation_target = preview
            .mutation_target()
            .map_err(|_| EmailError::PreviewBinding)?;
        Ok(Self {
            recipients,
            cc,
            bcc,
            subject: EmailSubject::new(preview.subject().to_owned())?,
            body: EmailBody::new(preview.body().to_owned())?,
            attach_quotation_pdf: preview.attach_quotation_pdf(),
            include_seven_day_link: preview.include_seven_day_link(),
            link_expiry_days: preview.link_expiry_days(),
            target_fingerprint: OperationTargetFingerprint::new(*mutation_target.as_bytes()),
        })
    }

    pub fn target_fingerprint(&self) -> OperationTargetFingerprint {
        self.target_fingerprint
    }

    pub fn recipients(&self) -> &[EmailAddress] {
        &self.recipients
    }

    pub fn cc(&self) -> &[EmailAddress] {
        &self.cc
    }

    pub fn bcc(&self) -> &[EmailAddress] {
        &self.bcc
    }

    pub fn subject(&self) -> &EmailSubject {
        &self.subject
    }

    pub fn body(&self) -> &EmailBody {
        &self.body
    }

    pub const fn attach_quotation_pdf(&self) -> bool {
        self.attach_quotation_pdf
    }

    pub const fn include_seven_day_link(&self) -> bool {
        self.include_seven_day_link
    }

    pub const fn link_expiry_days(&self) -> u16 {
        self.link_expiry_days
    }
}

/// Proof extracted from a consumed confirmation that authorizes email sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedEmailProof {
    owner: ParticipantId,
    mutation_target: MutationTargetFingerprint,
    preview_digest: PreviewDigest,
    workflow_revision: WorkflowRevision,
}

impl ConfirmedEmailProof {
    pub fn from_consumed(record: &ConfirmationRecord) -> Result<Self, EmailError> {
        if record.status() != ConfirmationStatus::Consumed {
            return Err(EmailError::Unauthorized);
        }
        if record.action() != ConfirmationAction::StartCalendarOrEmailAction {
            return Err(EmailError::Unauthorized);
        }
        Ok(Self {
            owner: record.owner(),
            mutation_target: record.mutation_target(),
            preview_digest: record.preview_digest(),
            workflow_revision: record
                .resulting_workflow_revision()
                .ok_or(EmailError::Unauthorized)?,
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

#[cfg(any(test, feature = "test-utils"))]
impl ConfirmedEmailProof {
    #[allow(clippy::too_many_arguments)]
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

// ── errors ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, thiserror::Error)]
pub enum EmailError {
    #[error("invalid email address")]
    InvalidAddress,
    #[error("invalid email subject")]
    InvalidSubject,
    #[error("invalid email body")]
    InvalidBody,
    #[error("too many recipients")]
    TooManyRecipients,
    #[error("too many CC recipients")]
    TooManyCc,
    #[error("too many BCC recipients")]
    TooManyBcc,
    #[error("email preview could not be bound to an operation target")]
    PreviewBinding,
    #[error("confirmation does not authorize this email action")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum SendError {
    #[error("confirmation does not authorize this email action")]
    Unauthorized,
    #[error("mutation target mismatch")]
    TargetMismatch,
    #[error("sanitized operation value invalid")]
    Sanitization,
}

pub(crate) fn failure(
    code: &'static str,
    summary: &'static str,
) -> Result<OperationFailure, SendError> {
    let code = FailureCode::new(code).map_err(|_| SendError::Sanitization)?;
    let summary = SanitizedSummary::new(summary).map_err(|_| SendError::Sanitization)?;
    Ok(OperationFailure::new(code, summary))
}
