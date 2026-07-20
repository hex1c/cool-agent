//! Version 1 quotation PDF renderer.
//!
//! Uses PDF standard Helvetica as the approved-by-prototype substitute
//! for the reference's SFUIDisplay/SFUIText subsets; embedded-font
//! substitution is a separate visual-overlay approval item.
//!
//! Image embedding (logo, signature) is deferred to a separate approval.
//! This version renders labeled slot rectangles instead.
//!
//! Tests are structural (page count, text presence), not pixel-diff.
//! Pixel golden gates are pending visual-overlay approval.

pub mod layout;
pub mod render;

pub use layout::{LayoutError, QuotationLayout, Region};
pub use render::{
    BankRenderData, CompanyRenderData, CustomerRenderData, LineItemRenderData, QuotationDocument,
    QuotationMetaRenderData, QuotationRenderer, RenderError, SummaryRenderData,
    TaxComponentRenderData,
};
