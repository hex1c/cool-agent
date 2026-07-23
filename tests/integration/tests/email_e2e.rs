#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! End-to-end email slice (Task 42).
//!
//! Orchestrates the cross-cutting confirmed Hostinger email path: an
//! AI-extracted `EmailResult` becomes a bound `EmailPreview` with a seven-day
//! S3 link, the digest binds recipient/content/link-expiry so a changed
//! recipient cannot consume a prior confirmation, and empty recipients are
//! rejected before any send. No AWS credentials or Docker required.

use application::email::EmailPreview;
use application::observability::{EnvironmentLabel, ObservabilityError};
use domain::contracts::EmailResult;

fn extracted_email(recipients: Vec<String>) -> EmailResult {
    EmailResult {
        recipients,
        cc: vec![],
        bcc: vec![],
        subject: "Your quotation Q-2025-001".to_owned(),
        body: "Please find your quotation attached.".to_owned(),
        attach_quotation_pdf: true,
        include_seven_day_link: true,
    }
}

#[test]
fn preview_binds_seven_day_link_and_pdf_attachment() {
    let result = extracted_email(vec!["customer@example.com".to_owned()]);
    let preview = EmailPreview::from_extraction(&result, 7).expect("preview builds");
    assert_eq!(preview.recipients(), &["customer@example.com".to_owned()]);
    assert!(preview.attach_quotation_pdf());
    assert!(preview.include_seven_day_link());
    assert_eq!(preview.link_expiry_days(), 7);
}

#[test]
fn changed_recipient_changes_digest() {
    let a = extracted_email(vec!["customer@example.com".to_owned()]);
    let b = extracted_email(vec!["finance@example.com".to_owned()]);
    let preview_a = EmailPreview::from_extraction(&a, 7).expect("a builds");
    let preview_b = EmailPreview::from_extraction(&b, 7).expect("b builds");
    assert_ne!(
        preview_a.digest().expect("digest a"),
        preview_b.digest().expect("digest b"),
        "recipient is bound to the confirmation digest"
    );
}

#[test]
fn empty_recipients_rejected_before_any_send() {
    let result = extracted_email(vec![]);
    let err = EmailPreview::from_extraction(&result, 7).expect_err("empty recipients rejected");
    let rendered = format!("{err}");
    assert!(
        rendered.to_ascii_lowercase().contains("recipient"),
        "error must mention recipient, got: {rendered}"
    );
}

#[test]
fn environment_label_rejects_invalid_values() {
    // Observability labels are constrained to known environment names —
    // arbitrary strings (which could smuggle secret material) are rejected.
    assert!(matches!(
        EnvironmentLabel::new("token=abc"),
        Err(ObservabilityError::InvalidEnvironmentLabel)
    ));
    assert!(EnvironmentLabel::new("development").is_ok());
}
