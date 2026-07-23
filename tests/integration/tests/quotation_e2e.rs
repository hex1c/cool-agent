#![deny(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! End-to-end quotation slice (Task 42).
//!
//! Orchestrates the cross-cutting read-only-to-confirmed quotation path across
//! crates: a Telegram forum mention normalizes to a `ForumTopic` route, a
//! configured `QuotationDocument` renders to canonical PDF bytes, and the same
//! raw webhook body always yields the same `update_id` (stable dedup key).
//! No AWS credentials or Docker required — pure structural orchestration.

use std::path::PathBuf;

use domain::routing::Route;
use pdf::render::{
    BankRenderData, CompanyRenderData, CustomerRenderData, LineItemRenderData, QuotationDocument,
    QuotationMetaRenderData, QuotationRenderer, SummaryRenderData, TaxComponentRenderData,
};
use telegram::normalize::{self, EventKind};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn telegram_fixture(name: &str) -> String {
    let path = repo_root()
        .join("tests/fixtures/telegram")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture {name} must exist at {}", path.display()))
}

fn valid_quotation_document() -> QuotationDocument {
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

#[test]
fn forum_mention_routes_to_topic_and_renders_canonical_pdf() {
    // 1. Webhook intake: a forum mention normalizes to a ForumTopic route.
    let json = telegram_fixture("mention");
    let update = normalize::normalize(json.as_bytes()).expect("mention normalizes");
    assert!(matches!(update.event, EventKind::Mention { .. }));
    let Route::ForumTopic { .. } = update.route else {
        panic!(
            "mention must route to a forum topic, got {:?}",
            update.route
        );
    };

    // 2. PDF render: a configured QuotationDocument renders to canonical bytes.
    let renderer = QuotationRenderer::default_v1().expect("default layout builds");
    let bytes = renderer
        .render(&valid_quotation_document())
        .expect("document renders");
    assert!(!bytes.is_empty(), "renderer must produce PDF bytes");
    assert!(
        bytes.starts_with(b"%PDF-"),
        "rendered artifact must be a PDF"
    );
}

#[test]
fn duplicate_webhook_update_id_is_stable_for_dedup() {
    let json = telegram_fixture("mention");
    let first = normalize::normalize(json.as_bytes()).expect("first normalizes");
    let second = normalize::normalize(json.as_bytes()).expect("second normalizes");
    assert_eq!(first.update_id, second.update_id);
}
