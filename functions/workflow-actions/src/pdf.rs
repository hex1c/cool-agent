#![deny(unsafe_code)]

//! PDF rendering handler (Task 34A).
//!
//! Deserializes a versioned [`PdfRenderEvent`] containing a
//! [`QuotationDocument`], renders it with the Version 1 layout, and returns
//! a [`PdfRenderResult`] with the rendered bytes (base64), SHA-256, and byte
//! length. Rendering is pure deterministic compute — it performs no I/O and
//! uses no adapter, so failures are terminal (invalid document or layout),
//! never retryable.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use pdf::render::{QuotationDocument, QuotationRenderer, RenderError};

use crate::EVENT_SCHEMA_VERSION;

/// Versioned event consumed by the `pdf` Lambda.
#[derive(Debug, Deserialize)]
pub struct PdfRenderEvent {
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,
    pub document: QuotationDocument,
}

/// Typed render result. `pdf_bytes_base64` carries the canonical PDF for the
/// downstream `delivery` handler. No secret or OAuth material is present.
#[derive(Debug, Serialize)]
pub struct PdfRenderResult {
    #[serde(rename = "sha256Hex")]
    pub sha256_hex: String,
    #[serde(rename = "byteLength")]
    pub byte_length: u64,
    #[serde(rename = "pdfBytesBase64")]
    pub pdf_bytes_base64: String,
}

/// Typed render failure. Variants map to terminal outcomes — there is no
/// retryable render failure because rendering is pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfRenderError {
    /// The event's `schemaVersion` does not match [`EVENT_SCHEMA_VERSION`].
    SchemaVersionMismatch,
    /// The document is missing a required field or fails validation.
    InvalidDocument { field: &'static str },
    /// The renderer could not be constructed from the default layout.
    InvalidLayout,
    /// PDF serialization produced no bytes.
    SerializeFailed,
    /// The event body could not be parsed.
    InvalidEvent,
}

impl std::fmt::Display for PdfRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaVersionMismatch => f.write_str("pdf render: schema version mismatch"),
            Self::InvalidDocument { field } => {
                write!(f, "pdf render: invalid document field {field:?}")
            }
            Self::InvalidLayout => f.write_str("pdf render: invalid default layout"),
            Self::SerializeFailed => f.write_str("pdf render: serialization produced no bytes"),
            Self::InvalidEvent => f.write_str("pdf render: invalid event body"),
        }
    }
}

impl std::error::Error for PdfRenderError {}

impl From<RenderError> for PdfRenderError {
    fn from(value: RenderError) -> Self {
        match value {
            RenderError::MissingRequiredField(field) => Self::InvalidDocument { field },
            RenderError::Serialize => Self::SerializeFailed,
            RenderError::Layout(_) => Self::InvalidLayout,
        }
    }
}

/// Pure processing function: render the document and return the typed result.
///
/// This is the unit-testable core; the Lambda `main` (in `src/bin/pdf.rs`)
/// deserializes the event from the Lambda payload and serializes the result
/// back into the response.
pub fn process_pdf(event: &PdfRenderEvent) -> Result<PdfRenderResult, PdfRenderError> {
    if event.schema_version != EVENT_SCHEMA_VERSION {
        return Err(PdfRenderError::SchemaVersionMismatch);
    }
    let renderer = QuotationRenderer::default_v1().map_err(|_| PdfRenderError::InvalidLayout)?;
    let bytes = renderer.render(&event.document)?;
    let byte_length = bytes.len() as u64;
    let sha256_hex = hex::encode(Sha256::digest(&bytes));
    let pdf_bytes_base64 = BASE64.encode(&bytes);
    Ok(PdfRenderResult {
        sha256_hex,
        byte_length,
        pdf_bytes_base64,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;
    use pdf::render::{
        BankRenderData, CompanyRenderData, CustomerRenderData, LineItemRenderData,
        QuotationMetaRenderData, SummaryRenderData, TaxComponentRenderData,
    };

    fn valid_document() -> QuotationDocument {
        QuotationDocument {
            company: CompanyRenderData {
                legal_name: "Novus Consulting Pvt Ltd".to_owned(),
                tax_id: Some("29AABCN1234M1Z5".to_owned()),
                address: vec![
                    "4th Floor, MG Road".to_owned(),
                    "Bengaluru 560001".to_owned(),
                ],
                contact: vec!["+91 80 1234 5678".to_owned()],
                logo_ref: None,
                signature_ref: None,
            },
            bank: BankRenderData {
                bank_name: "HDFC Bank".to_owned(),
                account_name: "Novus Consulting Pvt Ltd".to_owned(),
                account_number: "50100012345678".to_owned(),
                ifsc: "HDFC0001234".to_owned(),
                branch: Some("MG Road".to_owned()),
                upi_id: None,
            },
            meta: QuotationMetaRenderData {
                quotation_number: "Q-2025-001".to_owned(),
                quotation_date: "2025-01-15".to_owned(),
                place_of_supply: "Karnataka (29)".to_owned(),
                validity: "30 days".to_owned(),
                currency: "INR".to_owned(),
                copy_label: None,
            },
            customer: CustomerRenderData {
                name: "Acme Customer".to_owned(),
                tax_id: None,
                billing_address: vec!["Customer Address".to_owned()],
                shipping_address: vec!["Customer Address".to_owned()],
                dispatch_origin: None,
            },
            line_items: vec![LineItemRenderData {
                sequence: 1,
                description: "Consulting services".to_owned(),
                hsn_sac: "998314".to_owned(),
                quantity: "2".to_owned(),
                unit: "Nos".to_owned(),
                unit_rate: "50000.00".to_owned(),
                taxable_value: "100000.00".to_owned(),
                tax_rate: "18%".to_owned(),
                tax_amount: "18000.00".to_owned(),
                amount: "118000.00".to_owned(),
            }],
            summary: SummaryRenderData {
                item_count: 1,
                total_quantity: "2".to_owned(),
                taxable_total: "100000.00".to_owned(),
                tax_components: vec![TaxComponentRenderData {
                    label: "CGST+SGST".to_owned(),
                    rate: "18%".to_owned(),
                    amount: "18000.00".to_owned(),
                }],
                grand_total: "118000.00".to_owned(),
            },
            terms: vec!["Payment due within 15 days.".to_owned()],
            amount_in_words: "One lakh eighteen thousand only".to_owned(),
            signatory_label: "Authorised Signatory".to_owned(),
        }
    }

    fn event_with(document: QuotationDocument) -> PdfRenderEvent {
        PdfRenderEvent {
            schema_version: EVENT_SCHEMA_VERSION.to_owned(),
            document,
        }
    }

    #[test]
    fn valid_document_renders_with_checksum() {
        let event = event_with(valid_document());
        let result = process_pdf(&event).expect("valid document renders");
        assert!(!result.pdf_bytes_base64.is_empty());
        assert_eq!(
            result.byte_length as usize,
            BASE64.decode(&result.pdf_bytes_base64).unwrap().len()
        );
        assert_eq!(result.sha256_hex.len(), 64);
        assert!(result.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let mut event = event_with(valid_document());
        event.schema_version = "novus.workflow-actions.v0".to_owned();
        let err = process_pdf(&event).expect_err("mismatch rejected");
        assert!(matches!(err, PdfRenderError::SchemaVersionMismatch));
    }

    #[test]
    fn missing_required_field_is_terminal_failure() {
        let mut doc = valid_document();
        doc.company.legal_name = String::new();
        let event = event_with(doc);
        let err = process_pdf(&event).expect_err("invalid document rejected");
        assert!(matches!(err, PdfRenderError::InvalidDocument { .. }));
    }

    #[test]
    fn empty_line_items_is_terminal_failure() {
        let mut doc = valid_document();
        doc.line_items.clear();
        let event = event_with(doc);
        let err = process_pdf(&event).expect_err("empty line items rejected");
        assert!(matches!(err, PdfRenderError::InvalidDocument { .. }));
    }
}
