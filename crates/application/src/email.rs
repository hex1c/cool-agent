//! Confirmed email send orchestration (Task 37).
//!
//! Pure application layer that binds a consumed confirmation to the external
//! operation journal key for an SMTP email send via the shared Hostinger
//! mailbox.  Like [`crate::calendar`], every method is pure — it operates over
//! domain types and returns only the validated binding.  Persistence
//! (`ConfirmationRepository::consume_and_prepare_operation`) and provider
//! invocation (`ExternalOperationExecutor` with the
//! [`email::smtp::HostingerSmtpService`]) are the caller's responsibility.
//!
//! The binding enforces the Task 37 contract:
//! - The confirmation must be consumed with `StartCalendarOrEmailAction`.
//! - The operation key binds the email's mutation-target fingerprint via the
//!   confirmation's mutation-target fingerprint and the resulting workflow
//!   revision.
//! - Email sending uses [`OperationKind::SmtpSend`].
//! - A stale (already-consumed) confirmation is rejected by the domain
//!   `consume` step before this service is reached; a duplicate
//!   `consume_and_prepare_operation` is rejected by the repository conditional
//!   write.  In both cases zero provider writes occur, so emails cannot
//!   duplicate on retry.

use std::collections::HashSet;
use std::fmt::{Display, Formatter};

use serde::Serialize;
use sha2::{Digest, Sha256};

use domain::authorization::AuthorizedWorkflowAction;
use domain::confirmation::{
    ConfirmationAction, ConfirmationConsumeRequest, ConfirmationConsumption,
    ConfirmationIssueOutcome, ConfirmationIssueRequest, ConfirmationRecord,
    MutationTargetFingerprint, PreviewDigest, TopicMessageReference,
};
use domain::contracts::EmailResult;
use domain::idempotency::{IdempotencyKey, OperationKind, OperationTargetFingerprint};
use domain::identity::{ConfirmationId, MessageId, ParticipantId};
use domain::retry::RetryPolicy;
use domain::workflow::{WaitDeadline, Workflow, WorkflowRevision, WorkflowTimestamp};

use crate::repositories::{ConsumeAndPrepareError, ConsumeAndPrepareRequest};

const EMAIL_TARGET_LABEL: &str = "email-message-v1";
const MAX_SUBJECT_BYTES: usize = 256;
const MAX_BODY_BYTES: usize = 16_384;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_RECIPIENTS: usize = 50;
const MAX_CC: usize = 20;
const MAX_BCC: usize = 20;

/// Canonical email preview shown immediately before confirmation.
///
/// Every provider-relevant field is serialized into [`Self::digest`] and
/// [`Self::mutation_target`]. The email adapter can only build a request from
/// this type, so recipients, subject, body, attachment flags, and link expiry
/// cannot change after confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailPreview {
    recipients: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    subject: String,
    body: String,
    attach_quotation_pdf: bool,
    include_seven_day_link: bool,
    link_expiry_days: u16,
}

impl EmailPreview {
    /// Resolve AI extraction into the confirmation preview.
    pub fn from_extraction(
        result: &EmailResult,
        link_expiry_days: u16,
    ) -> Result<Self, EmailPreviewError> {
        Self::new(
            result.recipients.clone(),
            result.cc.clone(),
            result.bcc.clone(),
            result.subject.clone(),
            result.body.clone(),
            result.attach_quotation_pdf,
            result.include_seven_day_link,
            link_expiry_days,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut recipients: Vec<String>,
        cc: Vec<String>,
        bcc: Vec<String>,
        subject: String,
        body: String,
        attach_quotation_pdf: bool,
        include_seven_day_link: bool,
        link_expiry_days: u16,
    ) -> Result<Self, EmailPreviewError> {
        if recipients.is_empty() || recipients.len() > MAX_RECIPIENTS {
            return Err(EmailPreviewError::InvalidRecipients);
        }
        if cc.len() > MAX_CC {
            return Err(EmailPreviewError::TooManyCc);
        }
        if bcc.len() > MAX_BCC {
            return Err(EmailPreviewError::TooManyBcc);
        }
        if !(1..=30).contains(&link_expiry_days) {
            return Err(EmailPreviewError::InvalidLinkExpiry);
        }

        for email in recipients.iter().chain(cc.iter()).chain(bcc.iter()) {
            validate_email(email)?;
        }

        // Deduplicate within each list and ensure no cross-list overlaps.
        let dedup = |list: &mut Vec<String>| {
            let mut seen = HashSet::new();
            list.retain(|item| seen.insert(item.clone()));
        };
        dedup(&mut recipients);
        let mut cc = cc;
        let mut bcc = bcc;
        dedup(&mut cc);
        dedup(&mut bcc);

        if recipients.is_empty() {
            return Err(EmailPreviewError::InvalidRecipients);
        }

        // cc must not overlap with recipients or bcc
        let recipient_set: HashSet<&str> = recipients.iter().map(String::as_str).collect();
        let bcc_set: HashSet<&str> = bcc.iter().map(String::as_str).collect();
        if cc
            .iter()
            .any(|e| recipient_set.contains(e.as_str()) || bcc_set.contains(e.as_str()))
        {
            return Err(EmailPreviewError::OverlappingRecipientSets);
        }
        // bcc must not overlap with recipients
        if bcc.iter().any(|e| recipient_set.contains(e.as_str())) {
            return Err(EmailPreviewError::OverlappingRecipientSets);
        }

        validate_bounded_text(
            &subject,
            MAX_SUBJECT_BYTES,
            EmailPreviewError::InvalidSubject,
        )?;
        if subject.chars().any(char::is_control) {
            return Err(EmailPreviewError::InvalidSubject);
        }
        validate_body(&body, MAX_BODY_BYTES)?;

        Ok(Self {
            recipients,
            cc,
            bcc,
            subject,
            body,
            attach_quotation_pdf,
            include_seven_day_link,
            link_expiry_days,
        })
    }

    pub fn digest(&self) -> Result<PreviewDigest, EmailPreviewError> {
        let value =
            serde_json::to_value(self).map_err(|_| EmailPreviewError::SerializationFailed)?;
        let canonical =
            serde_json::to_vec(&value).map_err(|_| EmailPreviewError::SerializationFailed)?;
        let hash: [u8; 32] = Sha256::digest(canonical).into();
        Ok(PreviewDigest::new(hash))
    }

    pub fn mutation_target(&self) -> Result<MutationTargetFingerprint, EmailPreviewError> {
        let digest = self.digest()?;
        let combined = format!("{EMAIL_TARGET_LABEL}:{}", hex::encode(digest.as_bytes()));
        let hash: [u8; 32] = Sha256::digest(combined.as_bytes()).into();
        Ok(MutationTargetFingerprint::new(hash))
    }

    pub fn recipients(&self) -> &[String] {
        &self.recipients
    }

    pub fn cc(&self) -> &[String] {
        &self.cc
    }

    pub fn bcc(&self) -> &[String] {
        &self.bcc
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn body(&self) -> &str {
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmailPreviewError {
    #[error("invalid email recipients")]
    InvalidRecipients,
    #[error("too many CC recipients")]
    TooManyCc,
    #[error("too many BCC recipients")]
    TooManyBcc,
    #[error("invalid email address")]
    InvalidEmail,
    #[error("email recipient sets must not overlap")]
    OverlappingRecipientSets,
    #[error("invalid email subject")]
    InvalidSubject,
    #[error("invalid email body")]
    InvalidBody,
    #[error("link expiry must be 1-30 days")]
    InvalidLinkExpiry,
    #[error("email preview serialization failed")]
    SerializationFailed,
}

fn validate_bounded_text(
    value: &str,
    maximum: usize,
    error: EmailPreviewError,
) -> Result<(), EmailPreviewError> {
    if value.trim().is_empty() || value.len() > maximum {
        return Err(error);
    }
    Ok(())
}

fn validate_body(value: &str, maximum: usize) -> Result<(), EmailPreviewError> {
    if value.trim().is_empty() || value.len() > maximum {
        return Err(EmailPreviewError::InvalidBody);
    }
    // Allow newlines and carriage returns; reject other control characters.
    if value
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r')
    {
        return Err(EmailPreviewError::InvalidBody);
    }
    Ok(())
}

fn validate_email(value: &str) -> Result<(), EmailPreviewError> {
    if value.trim().is_empty()
        || value.len() > MAX_EMAIL_BYTES
        || value.chars().any(|c| c.is_ascii_control() || c == ' ')
    {
        return Err(EmailPreviewError::InvalidEmail);
    }
    let Some((local, domain)) = value.split_once('@') else {
        return Err(EmailPreviewError::InvalidEmail);
    };
    if local.is_empty()
        || domain.is_empty()
        || local.contains('@')
        || !domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return Err(EmailPreviewError::InvalidEmail);
    }
    Ok(())
}

/// Email-specific confirmation request. The action and mutation-target
/// label are fixed by the service and cannot be selected by callers.
pub struct IssueEmailConfirmationRequest<'a> {
    pub preview: &'a EmailPreview,
    pub confirmation_id: ConfirmationId,
    pub actor: ParticipantId,
    pub source_message: MessageId,
    pub deadline: WaitDeadline,
    pub timestamp: WorkflowTimestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum EmailConfirmationError {
    #[error(transparent)]
    Preview(#[from] EmailPreviewError),
    #[error(transparent)]
    Transition(#[from] domain::transition::TransitionError),
    #[error(transparent)]
    Confirmation(#[from] domain::confirmation::ConfirmationError),
}

/// Issues and consumes confirmations bound to the exact canonical Email
/// preview. No external provider operation is performed here.
pub struct EmailConfirmationService;

impl EmailConfirmationService {
    pub fn issue(
        workflow: &Workflow,
        request: IssueEmailConfirmationRequest<'_>,
    ) -> Result<ConfirmationIssueOutcome, EmailConfirmationError> {
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
        preview: &EmailPreview,
        source: TopicMessageReference,
        confirmed_at: WorkflowTimestamp,
    ) -> Result<ConfirmationConsumption, EmailConfirmationError> {
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

/// Rejected email operation binding.
#[derive(Debug)]
pub enum EmailFlowError {
    /// The confirmation-to-operation binding failed (revision, target, or
    /// action-kind mismatch).
    Binding(ConsumeAndPrepareError),
}

impl Display for EmailFlowError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Binding(error) => {
                write!(formatter, "email operation binding failed: {error}")
            }
        }
    }
}

impl std::error::Error for EmailFlowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(error) => Some(error),
        }
    }
}

impl From<ConsumeAndPrepareError> for EmailFlowError {
    fn from(value: ConsumeAndPrepareError) -> Self {
        Self::Binding(value)
    }
}

/// A validated confirmation-to-operation binding ready for atomic persistence
/// and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEmailSend {
    consume_request: ConsumeAndPrepareRequest,
    retry_policy: RetryPolicy,
}

impl PreparedEmailSend {
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

/// Stateless orchestration for confirmed email sends.
///
/// Binds a consumed [`ConfirmationConsumption`] to the [`IdempotencyKey`] the
/// external operation executor will reuse for every retry attempt.  The key
/// binds:
/// - the workflow id and **resulting** revision (after consumption),
/// - [`OperationKind::SmtpSend`], and
/// - the confirmation's mutation-target fingerprint as the
///   [`OperationTargetFingerprint`].
///
/// [`ConsumeAndPrepareRequest::new`] then validates that the confirmation
/// action pairs with `SmtpSend`, that the target matches, and that the
/// revisions agree — all before any I/O.
pub struct EmailOperationService;

impl EmailOperationService {
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
    ) -> Result<PreparedEmailSend, EmailFlowError> {
        let transition = consumption.transition();
        let resulting_revision: WorkflowRevision = transition.workflow.revision();
        let mutation_target = consumption.confirmation().mutation_target();
        let target: OperationTargetFingerprint =
            OperationTargetFingerprint::new(*mutation_target.as_bytes());
        let operation_key = IdempotencyKey::new(
            transition.workflow.id().clone(),
            resulting_revision,
            OperationKind::SmtpSend,
            target,
        );
        let consume_request = ConsumeAndPrepareRequest::new(consumption, operation_key)?;
        Ok(PreparedEmailSend {
            consume_request,
            retry_policy,
        })
    }
}
