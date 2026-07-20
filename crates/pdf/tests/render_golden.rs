//! Structural tests for the Version 1 quotation PDF renderer.
//!
//! These tests assert page count and text presence, NOT pixel positions.
//! Pixel/tolerance golden gates are pending visual-overlay approval per
//! `docs/quotation-layout.md`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use pdf::QuotationRenderer;
use pdf::render::{QuotationDocument, RenderError};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Fixture loading
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FixtureFile {
    cases: FixtureCases,
}

#[derive(Debug, Deserialize)]
struct FixtureCases {
    normal: QuotationDocument,
    maximum_line: QuotationDocument,
    wrapping: QuotationDocument,
    overflow: QuotationDocument,
}

fn load_fixtures() -> FixtureFile {
    let json = include_str!("../../../tests/fixtures/quotation/render-cases.json");
    serde_json::from_str(json).expect("render-cases.json must deserialize")
}

fn renderer() -> QuotationRenderer {
    QuotationRenderer::default_v1().expect("default layout loads")
}

/// Join all text chunks across a page into one lowercase string.
fn page_text(chunks: &[String]) -> String {
    chunks.join(" ").to_lowercase()
}

/// True if `needle` appears in any page's joined text.
fn any_page_contains(all_pages: &[Vec<String>], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    all_pages
        .iter()
        .any(|chunks| page_text(chunks).contains(&needle))
}

/// Index of the first page whose joined text contains `needle`.
fn page_containing(all_pages: &[Vec<String>], needle: &str) -> Option<usize> {
    let needle = needle.to_lowercase();
    all_pages
        .iter()
        .position(|chunks| page_text(chunks).contains(&needle))
}

/// Count occurrences of `needle` across all pages' joined text.
fn count_occurrences(all_pages: &[Vec<String>], needle: &str) -> usize {
    let needle = needle.to_lowercase();
    all_pages
        .iter()
        .map(|chunks| page_text(chunks).matches(&needle).count())
        .sum()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn normal_case_renders_single_page() {
    let fixture = load_fixtures();
    let renderer = renderer();
    let (doc, bytes) = renderer
        .render_document(&fixture.cases.normal)
        .expect("normal renders");
    let plan = renderer.plan_pages(&fixture.cases.normal).expect("plan");
    assert_eq!(plan.pages.len(), 1, "normal case must fit on one page");
    assert!(!bytes.is_empty(), "bytes must be produced");
    assert!(bytes.starts_with(b"%PDF-"), "output must be a PDF");

    let text = doc.extract_text();
    let joined = text
        .iter()
        .flat_map(|p| p.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let joined_lower = joined.to_lowercase();
    assert!(joined_lower.contains("acme test services"));
    assert!(joined_lower.contains("acme-2026-0001"));
    assert!(joined_lower.contains("globex corporation"));
    assert!(joined_lower.contains("consulting services - architecture review"));
    assert!(joined_lower.contains("grand total"));
    assert!(joined_lower.contains("44840.00"));
    assert!(joined_lower.contains("amount in words"));
    assert!(joined_lower.contains("terms & conditions"));
    assert_eq!(doc.pages.len(), 1);
}

#[test]
fn maximum_line_case_fits_without_truncation() {
    let fixture = load_fixtures();
    let renderer = renderer();
    let (doc, _bytes) = renderer
        .render_document(&fixture.cases.maximum_line)
        .expect("maximum renders");
    let plan = renderer
        .plan_pages(&fixture.cases.maximum_line)
        .expect("plan");
    // Seven single-line items fit within the 7-baseline body region.
    assert_eq!(plan.pages.len(), 1, "7 items must fit on one page");
    let text = doc.extract_text();
    // Every item's amount must appear (no truncation).
    for item in &fixture.cases.maximum_line.line_items {
        assert!(
            any_page_contains(&text, &item.amount),
            "amount {} must appear (no truncation)",
            item.amount
        );
    }
}

#[test]
fn wrapping_case_description_wraps_and_keeps_financials() {
    let fixture = load_fixtures();
    let renderer = renderer();
    let (doc, _bytes) = renderer
        .render_document(&fixture.cases.wrapping)
        .expect("wrapping renders");
    let text = doc.extract_text();
    // The long description text appears across the page(s).
    assert!(
        any_page_contains(&text, "comprehensive cloud migration assessment"),
        "wrapped description text must appear"
    );
    // Financial amount is not truncated.
    assert!(
        any_page_contains(&text, "59000.00"),
        "wrapping item amount must appear"
    );
}

#[test]
fn overflow_case_continues_to_second_page_without_truncation() {
    let fixture = load_fixtures();
    let renderer = renderer();
    let (doc, _bytes) = renderer
        .render_document(&fixture.cases.overflow)
        .expect("overflow renders");
    let plan = renderer.plan_pages(&fixture.cases.overflow).expect("plan");
    assert!(
        plan.pages.len() >= 2,
        "20 items must overflow to >= 2 pages"
    );
    let text = doc.extract_text();
    assert_eq!(doc.pages.len(), plan.pages.len());
    // Continuation pages repeat the quotation number (rule 1).
    assert!(
        count_occurrences(&text, "acme-2026-0004") >= 2,
        "quotation number must repeat on continuation pages"
    );
    // Continuation pages repeat the table header (Description label).
    assert!(
        count_occurrences(&text, "description") >= 2,
        "table header must repeat on continuation pages"
    );
    // Last item's amount appears (no truncation at page break).
    let last = fixture.cases.overflow.line_items.last().expect("has items");
    assert!(
        any_page_contains(&text, &last.amount),
        "last item amount must appear"
    );
    // Grand total appears on the final page.
    let total_page = text.len().saturating_sub(1);
    let total_text = page_text(text.get(total_page).unwrap_or(&Vec::new()));
    assert!(
        total_text.contains("2360.00"),
        "grand total must be on the final page, got page text: {total_text}"
    );
}

#[test]
fn missing_required_field_fails() {
    let mut fixture = load_fixtures();
    fixture.cases.normal.company.legal_name.clear();
    let renderer = renderer();
    let result = renderer.render_document(&fixture.cases.normal);
    assert!(matches!(
        result,
        Err(RenderError::MissingRequiredField("company.legal_name"))
    ));
}

#[test]
fn pathological_single_item_splits_at_line_boundary() {
    let fixture = load_fixtures();
    let mut doc = fixture.cases.normal.clone();
    // One item whose description alone exceeds a page.
    let long_desc = "word ".repeat(600);
    doc.line_items = vec![pdf::render::LineItemRenderData {
        sequence: 1,
        description: long_desc,
        hsn_sac: "998314".to_string(),
        quantity: "1".to_string(),
        unit: "lot".to_string(),
        unit_rate: "100.00".to_string(),
        taxable_value: "100.00".to_string(),
        tax_rate: "18%".to_string(),
        tax_amount: "18.00".to_string(),
        amount: "7777.77".to_string(),
    }];
    doc.summary.item_count = 1;
    doc.summary.total_quantity = "1".to_string();
    doc.summary.taxable_total = "100.00".to_string();
    doc.summary.grand_total = "118.00".to_string();
    let renderer = renderer();
    let (pdf_doc, _bytes) = renderer
        .render_document(&doc)
        .expect("pathological renders");
    let plan = renderer.plan_pages(&doc).expect("plan");
    assert!(
        plan.pages.len() >= 2,
        "pathological single item must span >= 2 pages"
    );
    let text = pdf_doc.extract_text();
    // The line-item amount appears (financial data not lost).
    assert!(
        any_page_contains(&text, "7777.77"),
        "line-item amount must appear somewhere"
    );
    // Rule 4: the line-item amount appears exactly once (not repeated across pages).
    assert_eq!(
        count_occurrences(&text, "7777.77"),
        1,
        "line-item amount must appear exactly once across all pages"
    );
}

#[test]
fn financial_summary_moved_intact_when_not_fitting() {
    let fixture = load_fixtures();
    let renderer = renderer();
    let (doc, _bytes) = renderer
        .render_document(&fixture.cases.overflow)
        .expect("overflow renders");
    let text = doc.extract_text();
    // Rule 5: grand total and amount-in-words are on the SAME page.
    let grand_total_page = page_containing(&text, "2360.00").expect("grand total page found");
    let words_page = page_containing(&text, "two thousand three hundred sixty")
        .expect("amount in words page found");
    assert_eq!(
        grand_total_page, words_page,
        "grand total and amount-in-words must be on the same page (rule 5)"
    );
}
