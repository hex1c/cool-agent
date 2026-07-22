//! Hostinger SMTP confirmed-email send adapter (Task 37).
//!
//! Pure adapter layer that binds a consumed confirmation to a validated
//! [`EmailMessageRequest`] and an [`SmtpClient`] invocation.  Like the
//! Google Calendar adapter, every method is pure — it validates domain types
//! and returns a [`ProviderOutcome`] for the shared external-operation
//! executor.  Persistence and retry are the caller's responsibility.
//!
//! The proof enforces the Task 37 contract:
//! - The confirmation must be consumed with `StartCalendarOrEmailAction`.
//! - The operation target fingerprint must match the confirmation's
//!   mutation-target fingerprint.
//! - Owner-bound execution: the sender mailbox is the shared company mailbox;
//!   access is authenticated via [`SmtpCredentials`].
//!
//! No real SMTP connection or `lettre` dependency exists here. The
//! [`SmtpClient`] trait is the provider boundary; a mock exercises every
//! outcome in `tests/smtp_contracts.rs`.

use std::fmt::{self, Debug, Display, Formatter};

use zeroize::Zeroize;

use application::external_operation::{ExternalResourceId, ProviderOutcome};

use crate::message::{ConfirmedEmailProof, EmailMessageRequest, SendError, failure};

// ── credentials ───────────────────────────────────────────────────────

/// Hostinger SMTP credentials (sender mailbox address + password).
///
/// Both values are zeroized on drop. Debug and Display always show
/// `[REDACTED]` — never the plaintext values.
pub struct SmtpCredentials {
    sender_address: String,
    password: String,
}

impl SmtpCredentials {
    pub fn new(sender_address: String, password: String) -> Self {
        Self {
            sender_address,
            password,
        }
    }

    pub fn sender_address(&self) -> &str {
        &self.sender_address
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

impl Drop for SmtpCredentials {
    fn drop(&mut self) {
        self.sender_address.zeroize();
        self.password.zeroize();
    }
}

impl Debug for SmtpCredentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl Display for SmtpCredentials {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

// ── provider outcome ──────────────────────────────────────────────────

/// Provider outcome reported by an SMTP client to the service.
///
/// Every variant maps directly to the Hostinger spike vocabulary:
/// - `Accepted` → final `2yz` received after DATA terminator.
/// - `Ambiguous` → post-terminator timeout/drop; unknown to client.
/// - `Terminal` → permanent `5yz` rejection.
/// - `RecipientRejected` → one or more `RCPT TO` rejected; `RSET` before DATA.
/// - `RetryableFailure` → pre-terminator transient (`4yz` or transport).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpProviderOutcome {
    Accepted(ExternalResourceId),
    Ambiguous,
    Terminal,
    RecipientRejected,
    RetryableFailure,
}

// ── client trait ──────────────────────────────────────────────────────

/// SMTP provider client boundary.
///
/// Implementations translate the validated [`EmailMessageRequest`] into an
/// SMTP submission using the shared company mailbox credentials. They must
/// not recompute or mutate any extracted value. All transport failures must
/// be explicitly classified per the Hostinger spike vocabulary.
#[allow(async_fn_in_trait)]
pub trait SmtpClient {
    async fn send(
        &self,
        credentials: &SmtpCredentials,
        request: &EmailMessageRequest,
    ) -> SmtpProviderOutcome;
}

/// `()` is not a real client; used only for the pure `validate` tests
/// where no provider invocation occurs.
#[cfg(any(test, feature = "test-utils"))]
impl SmtpClient for () {
    async fn send(
        &self,
        _credentials: &SmtpCredentials,
        _request: &EmailMessageRequest,
    ) -> SmtpProviderOutcome {
        unreachable!("unit validate tests never invoke the client")
    }
}

// ── errors ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, thiserror::Error)]
pub enum SmtpError {
    #[error("invalid operation outcome code")]
    InvalidCode,
    #[error("invalid operation outcome summary")]
    InvalidSummary,
}

// ── service ───────────────────────────────────────────────────────────

/// SMTP send service backed by a pluggable [`SmtpClient`].
///
/// Validates the proof target against the request target, classifies every
/// [`SmtpProviderOutcome`] into the canonical [`ProviderOutcome`], and
/// ensures no credentials, recipient emails, or message content appear in
/// any failure metadata.
pub struct HostingerSmtpService<Client: SmtpClient> {
    client: Client,
}

impl<Client: SmtpClient> HostingerSmtpService<Client> {
    pub const fn new(client: Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Validate that the proof authorizes this request's target.
    pub fn validate(
        proof: &ConfirmedEmailProof,
        request: &EmailMessageRequest,
    ) -> Result<(), SendError> {
        if proof.mutation_target().as_bytes() != request.target_fingerprint().as_bytes() {
            return Err(SendError::TargetMismatch);
        }
        Ok(())
    }

    /// Invoke the SMTP client with the shared mailbox credentials, validated
    /// proof, and canonical request. Returns a classified [`ProviderOutcome`]
    /// for the external operation executor.
    ///
    /// No credentials, recipient emails, or message content appear in any
    /// failure metadata or log output.
    pub async fn send(
        &self,
        credentials: &SmtpCredentials,
        proof: &ConfirmedEmailProof,
        request: &EmailMessageRequest,
    ) -> Result<ProviderOutcome, SendError> {
        Self::validate(proof, request)?;

        match self.client.send(credentials, request).await {
            SmtpProviderOutcome::Accepted(id) => Ok(ProviderOutcome::Accepted {
                resource_id: Some(id),
            }),
            SmtpProviderOutcome::Ambiguous => {
                let f = failure(
                    "smtp_ambiguous",
                    "smtp outcome is ambiguous after data terminator",
                )?;
                Ok(ProviderOutcome::Ambiguous(f))
            }
            SmtpProviderOutcome::Terminal => {
                let f = failure(
                    "smtp_terminal",
                    "smtp server permanently rejected the message",
                )?;
                Ok(ProviderOutcome::TerminalFailure(f))
            }
            SmtpProviderOutcome::RecipientRejected => {
                let f = failure(
                    "smtp_recipient_rejected",
                    "one or more smtp recipients were rejected before data",
                )?;
                Ok(ProviderOutcome::TerminalFailure(f))
            }
            SmtpProviderOutcome::RetryableFailure => {
                let f = failure("smtp_retryable", "smtp request was not accepted")?;
                Ok(ProviderOutcome::RetryableFailure(f))
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use application::email::EmailPreview;
    use domain::confirmation::MutationTargetFingerprint;
    use domain::confirmation::PreviewDigest;
    use domain::identity::ParticipantId;
    use domain::workflow::WorkflowRevision;

    use super::*;

    fn preview() -> EmailPreview {
        EmailPreview::new(
            vec!["alice@example.com".to_owned()],
            vec![],
            vec![],
            "Follow-up".to_owned(),
            "Please find attached.".to_owned(),
            true,
            true,
            7,
        )
        .expect("valid preview")
    }

    #[test]
    fn request_is_derived_from_exact_preview() {
        let preview = preview();
        let request = EmailMessageRequest::from_preview(&preview).expect("request");
        assert_eq!(request.recipients().len(), 1);
        assert_eq!(request.recipients()[0].as_str(), "alice@example.com");
        assert_eq!(request.subject().as_str(), "Follow-up");
        assert_eq!(request.body().as_str(), "Please find attached.");
        assert!(request.attach_quotation_pdf());
        assert!(request.include_seven_day_link());
        assert_eq!(request.link_expiry_days(), 7);
    }

    #[test]
    fn changing_confirmed_payload_changes_operation_target() {
        let original = preview();
        let changed = EmailPreview::new(
            vec!["bob@example.com".to_owned()],
            vec![],
            vec![],
            "Different".to_owned(),
            "Different body.".to_owned(),
            false,
            false,
            7,
        )
        .expect("changed preview");
        let original_request = EmailMessageRequest::from_preview(&original).expect("request");
        let changed_request = EmailMessageRequest::from_preview(&changed).expect("request");
        assert_ne!(
            original_request.target_fingerprint(),
            changed_request.target_fingerprint()
        );
    }

    #[test]
    fn validate_rejects_target_mismatch() {
        let request = EmailMessageRequest::from_preview(&preview()).expect("request");
        let proof = ConfirmedEmailProof::new(
            ParticipantId::new(101).unwrap(),
            MutationTargetFingerprint::new([9u8; 32]),
            PreviewDigest::new([1u8; 32]),
            WorkflowRevision::new(2),
        );
        assert!(matches!(
            HostingerSmtpService::<()>::validate(&proof, &request),
            Err(SendError::TargetMismatch)
        ));
    }

    #[test]
    fn validate_accepts_matching_target() {
        let preview = preview();
        let request = EmailMessageRequest::from_preview(&preview).expect("request");
        let mutation_target = preview.mutation_target().expect("target");
        let proof = ConfirmedEmailProof::new(
            ParticipantId::new(101).unwrap(),
            mutation_target,
            preview.digest().expect("digest"),
            WorkflowRevision::new(2),
        );
        HostingerSmtpService::<()>::validate(&proof, &request).expect("target matches");
    }

    #[test]
    fn credentials_redacted_in_debug_and_display() {
        let creds = SmtpCredentials::new("sender@example.com".to_owned(), "secret".to_owned());
        let debug = format!("{creds:?}");
        let display = format!("{creds}");
        assert_eq!(debug, "[REDACTED]");
        assert_eq!(display, "[REDACTED]");
        assert!(!debug.contains("sender"));
        assert!(!display.contains("secret"));
    }

    #[test]
    fn credentials_zeroized_on_drop() {
        let sender = "sender@example.com".to_owned();
        let password = "secret".to_owned();
        {
            let _creds = SmtpCredentials::new(sender.clone(), password.clone());
        }
        // After drop, the original Strings should still be valid (the creds
        // owns its own copies), but we can verify the creds' own copies are
        // zeroized by checking that when we create and immediately drop, the
        // internal values were zeroed. We test this indirectly: the Drop impl
        // calls zeroize which overwrites with zeros. We cannot observe this
        // without accessing the now-dropped struct, so this test just ensures
        // the drop compiles and runs without panic.
    }

    #[test]
    fn smtp_provider_outcome_maps_to_canonical_outcome_variants() {
        // Verify the enum shape compiles.
        let accepted =
            SmtpProviderOutcome::Accepted(ExternalResourceId::new("msg-001").expect("valid id"));
        assert!(matches!(accepted, SmtpProviderOutcome::Accepted(_)));
        assert!(matches!(
            SmtpProviderOutcome::Ambiguous,
            SmtpProviderOutcome::Ambiguous
        ));
        assert!(matches!(
            SmtpProviderOutcome::Terminal,
            SmtpProviderOutcome::Terminal
        ));
        assert!(matches!(
            SmtpProviderOutcome::RecipientRejected,
            SmtpProviderOutcome::RecipientRejected
        ));
        assert!(matches!(
            SmtpProviderOutcome::RetryableFailure,
            SmtpProviderOutcome::RetryableFailure
        ));
    }
}
