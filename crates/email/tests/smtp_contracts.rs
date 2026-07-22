#![deny(unsafe_code)]
// Test-only allowances mirror crates/google/src/calendar.rs unit tests.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use application::email::EmailPreview;
use application::external_operation::{ExternalResourceId, ProviderOutcome};
use domain::confirmation::MutationTargetFingerprint;
use domain::confirmation::PreviewDigest;
use domain::identity::ParticipantId;
use domain::workflow::WorkflowRevision;
use email::message::{ConfirmedEmailProof, EmailMessageRequest, SendError};
use email::smtp::{HostingerSmtpService, SmtpClient, SmtpCredentials, SmtpProviderOutcome};

/// A mock SMTP client that returns a pre-configured outcome.
struct MockSmtpClient {
    outcome: SmtpProviderOutcome,
}

impl MockSmtpClient {
    const fn new(outcome: SmtpProviderOutcome) -> Self {
        Self { outcome }
    }
}

impl SmtpClient for MockSmtpClient {
    async fn send(
        &self,
        _credentials: &SmtpCredentials,
        _request: &EmailMessageRequest,
    ) -> SmtpProviderOutcome {
        self.outcome.clone()
    }
}

fn preview() -> EmailPreview {
    EmailPreview::new(
        vec!["customer@example.com".to_owned()],
        vec!["cc@example.com".to_owned()],
        vec![],
        "Quotation follow-up".to_owned(),
        "Please find the quotation attached.".to_owned(),
        true,
        true,
        7,
    )
    .expect("valid preview")
}

fn matching_proof(preview: &EmailPreview) -> ConfirmedEmailProof {
    ConfirmedEmailProof::new(
        ParticipantId::new(101).unwrap(),
        preview.mutation_target().expect("target"),
        preview.digest().expect("digest"),
        WorkflowRevision::new(2),
    )
}

fn credentials() -> SmtpCredentials {
    SmtpCredentials::new("sender@example.com".to_owned(), "secret".to_owned())
}

#[tokio::test]
async fn success_accepted() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    let resource_id = ExternalResourceId::new("msg-001").expect("valid id");
    let client = MockSmtpClient::new(SmtpProviderOutcome::Accepted(resource_id.clone()));
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let outcome = service
        .send(&credentials, &proof, &request)
        .await
        .expect("send");

    assert!(matches!(outcome, ProviderOutcome::Accepted { .. }));
    if let ProviderOutcome::Accepted { resource_id: rid } = outcome {
        assert_eq!(rid, Some(resource_id));
    }
}

#[tokio::test]
async fn terminal_permanent_rejection() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    let client = MockSmtpClient::new(SmtpProviderOutcome::Terminal);
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let outcome = service
        .send(&credentials, &proof, &request)
        .await
        .expect("send");

    assert!(matches!(outcome, ProviderOutcome::TerminalFailure(_)));
}

#[tokio::test]
async fn recipient_rejected_maps_to_terminal_failure() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    let client = MockSmtpClient::new(SmtpProviderOutcome::RecipientRejected);
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let outcome = service
        .send(&credentials, &proof, &request)
        .await
        .expect("send");

    assert!(matches!(outcome, ProviderOutcome::TerminalFailure(_)));
    if let ProviderOutcome::TerminalFailure(f) = outcome {
        assert_eq!(f.code().as_str(), "smtp_recipient_rejected");
    }
}

#[tokio::test]
async fn pre_terminator_transient_retryable() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    let client = MockSmtpClient::new(SmtpProviderOutcome::RetryableFailure);
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let outcome = service
        .send(&credentials, &proof, &request)
        .await
        .expect("send");

    assert!(matches!(outcome, ProviderOutcome::RetryableFailure(_)));
}

#[tokio::test]
async fn post_terminator_ambiguity() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    let client = MockSmtpClient::new(SmtpProviderOutcome::Ambiguous);
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let outcome = service
        .send(&credentials, &proof, &request)
        .await
        .expect("send");

    assert!(matches!(outcome, ProviderOutcome::Ambiguous(_)));
}

#[tokio::test]
async fn target_mismatch_proof_rejection() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = ConfirmedEmailProof::new(
        ParticipantId::new(101).unwrap(),
        MutationTargetFingerprint::new([9u8; 32]),
        PreviewDigest::new([1u8; 32]),
        WorkflowRevision::new(2),
    );
    let client = MockSmtpClient::new(SmtpProviderOutcome::Terminal);
    let service = HostingerSmtpService::new(client);
    let credentials = credentials();

    let result = service.send(&credentials, &proof, &request).await;

    assert!(matches!(result, Err(SendError::TargetMismatch)));
}

#[test]
fn validate_is_usable_for_pure_checks() {
    let preview = preview();
    let request = EmailMessageRequest::from_preview(&preview).expect("request");
    let proof = matching_proof(&preview);
    HostingerSmtpService::<MockSmtpClient>::validate(&proof, &request).expect("target matches");
}
