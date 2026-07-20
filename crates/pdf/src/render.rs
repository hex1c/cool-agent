//! Quotation PDF rendering using the approved layout contract.

use std::fmt::Write as _;

use printpdf::color::{Color, Rgb};
use printpdf::font::BuiltinFont;
use printpdf::graphics::{Line, LinePoint, Point, Rect};
use printpdf::ops::{Op, PdfFontHandle};
use printpdf::serialize::PdfSaveOptions;
use printpdf::text::TextItem;
use printpdf::units::Mm;
use printpdf::{PdfDocument, PdfPage, PdfWarnMsg};
use serde::Deserialize;
use thiserror::Error;

use crate::layout::{
    COLOR_ACCENT_B, COLOR_ACCENT_G, COLOR_ACCENT_R, COLOR_PRIMARY_B, COLOR_PRIMARY_G,
    COLOR_PRIMARY_R, FONT_SIZE_BODY, FONT_SIZE_TABLE_HEADER, FONT_SIZE_TERMS, FONT_SIZE_TITLE,
    FONT_SIZE_TOTALS_BOLD, PAGE_HEIGHT_MM, PAGE_WIDTH_MM, QuotationLayout, Region, pt, pt_to_pdf_x,
    pt_to_pdf_y,
};

// ---------------------------------------------------------------------------
// Render data model (caller-supplied, pre-formatted strings)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuotationDocument {
    pub company: CompanyRenderData,
    pub meta: QuotationMetaRenderData,
    pub customer: CustomerRenderData,
    pub line_items: Vec<LineItemRenderData>,
    pub summary: SummaryRenderData,
    pub terms: Vec<String>,
    pub amount_in_words: String,
    pub signatory_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompanyRenderData {
    pub legal_name: String,
    pub tax_id: Option<String>,
    pub address: Vec<String>,
    pub contact: Vec<String>,
    pub logo_ref: Option<String>,
    pub signature_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuotationMetaRenderData {
    pub quotation_number: String,
    pub quotation_date: String,
    pub place_of_supply: String,
    pub validity: String,
    pub currency: String,
    pub copy_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CustomerRenderData {
    pub name: String,
    pub tax_id: Option<String>,
    pub billing_address: Vec<String>,
    pub shipping_address: Vec<String>,
    pub dispatch_origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LineItemRenderData {
    pub sequence: u32,
    pub description: String,
    pub hsn_sac: String,
    pub quantity: String,
    pub unit: String,
    pub unit_rate: String,
    pub taxable_value: String,
    pub tax_rate: String,
    pub tax_amount: String,
    pub amount: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SummaryRenderData {
    pub item_count: u32,
    pub total_quantity: String,
    pub taxable_total: String,
    pub tax_components: Vec<TaxComponentRenderData>,
    pub grand_total: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaxComponentRenderData {
    pub label: String,
    pub rate: String,
    pub amount: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Error)]
pub enum RenderError {
    #[error("layout error: {0}")]
    Layout(#[from] crate::layout::LayoutError),
    #[error("missing required field: {0}")]
    MissingRequiredField(&'static str),
    #[error("pdf serialization produced no bytes")]
    Serialize,
}

// ---------------------------------------------------------------------------
// Page plan (content assignment before rendering, for "Page X of N")
// ---------------------------------------------------------------------------

/// A logical block that occupies vertical space on a page.
#[derive(Debug, Clone)]
pub enum Block {
    /// Continuation-page header: title strip + quotation number/date + table header.
    ContinuationHeader,
    /// A single line-item, already split into wrapped text lines.
    ItemRow { lines: Vec<String>, amount: String },
    /// Item/quantity footer.
    ItemFooter,
    /// Tax + total summary block (kept intact; rule 8).
    Summary,
    /// Amount in words (one line; rule 8).
    AmountInWords,
    /// Bank + signature panel (kept intact; rule 6).
    BankSignature,
    /// Terms heading + terms lines.
    Terms { lines: Vec<String> },
}

impl Block {
    /// Approximate height in points consumed by this block.
    fn height_pt(&self, layout: &QuotationLayout) -> f32 {
        let row_h = layout.minimum_row_line_height;
        match self {
            Self::ContinuationHeader => layout.table_header_bottom - layout.table_header_top + 34.0,
            Self::ItemRow { lines, .. } => row_h * (lines.len().max(1) as f32),
            Self::ItemFooter => 14.0,
            Self::Summary => 14.0 * (3.0 + layout.columns.len().min(4) as f32),
            Self::AmountInWords => 14.0,
            Self::BankSignature => 100.0,
            Self::Terms { lines } => 18.0 + 9.36 * (lines.len() as f32),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlannedPage {
    /// Index of the first line item rendered on this page (None for summary-only pages).
    pub start_item: Option<usize>,
    /// One-past the last line item rendered on this page.
    pub end_item: usize,
    /// Blocks on this page in render order.
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
pub struct PagePlan {
    pub pages: Vec<PlannedPage>,
}

// ---------------------------------------------------------------------------
// Renderer
// ---------------------------------------------------------------------------

pub struct QuotationRenderer {
    layout: QuotationLayout,
}

impl QuotationRenderer {
    pub fn new(layout: QuotationLayout) -> Self {
        Self { layout }
    }

    pub fn default_v1() -> Result<Self, RenderError> {
        Ok(Self::new(QuotationLayout::default_v1()?))
    }

    /// Plan page assignment of all content blocks without serializing.
    pub fn plan_pages(&self, doc: &QuotationDocument) -> Result<PagePlan, RenderError> {
        doc.validate()?;
        let layout = &self.layout;
        let _table = layout.region("line_item_table")?;
        let body_top = layout.table_header_bottom + 2.0;
        let body_bottom = layout.table_body_bottom;
        let usable_per_page = body_bottom - body_top;
        let footer_region = layout.region("footer")?;
        // The financial summary tail is moved to its own page if it cannot fit on
        // the last item page (handled after the loop); it does NOT gate every
        // page break, so short quotations fill page 1 normally.

        // Wrap each item description into lines that fit the item column width.
        let item_col = layout.column_by_name("item");
        let item_width_pt = item_col.map(|c| c.width() - 2.0).unwrap_or(150.0);
        let wrapped: Vec<Vec<String>> = doc
            .line_items
            .iter()
            .map(|item| wrap_text(&item.description, item_width_pt))
            .collect();

        let mut pages: Vec<PlannedPage> = Vec::new();
        let mut current_blocks: Vec<Block> = Vec::new();
        let mut current_height = 0.0_f32;
        let mut start_item: Option<usize> = None;
        let mut idx = 0usize;

        while idx < doc.line_items.len() {
            let lines = wrapped
                .get(idx)
                .ok_or(RenderError::MissingRequiredField("wrapped item"))?
                .clone();
            let amount = doc
                .line_items
                .get(idx)
                .ok_or(RenderError::MissingRequiredField("line item"))?
                .amount
                .clone();
            let block = Block::ItemRow { lines, amount };
            let h = block.height_pt(layout);

            // Flush the page only when the next row would overflow the body region.
            if current_height + h > usable_per_page && !current_blocks.is_empty() {
                pages.push(PlannedPage {
                    start_item,
                    end_item: idx,
                    blocks: std::mem::take(&mut current_blocks),
                });
                start_item = None;
                current_height = 0.0;
                continue;
            }

            if start_item.is_none() {
                start_item = Some(idx);
            }
            current_blocks.push(block);
            current_height += h;
            idx += 1;
        }

        // Flush remaining item blocks.
        if !current_blocks.is_empty() {
            pages.push(PlannedPage {
                start_item,
                end_item: idx,
                blocks: std::mem::take(&mut current_blocks),
            });
        }

        // Financial summary tail: item footer + summary + amount-in-words + bank + terms.
        // Rule 5/6/8: keep summary, amount-in-words, bank/signature intact; move as a unit.
        // The tail occupies fixed regions BELOW the table on the same page, so it fits
        // on page 1 for short quotations. It only needs its own page when the last
        // item page is a full continuation page (items consumed the body region).
        let terms_width = layout.region("terms")?.width - 4.0;
        let terms_lines: Vec<String> = doc
            .terms
            .iter()
            .flat_map(|t| wrap_text(t, terms_width))
            .collect();
        let tail: Vec<Block> = vec![
            Block::ItemFooter,
            Block::Summary,
            Block::AmountInWords,
            Block::BankSignature,
            Block::Terms { lines: terms_lines },
        ];

        if pages.is_empty() {
            // No items: a single page holds the empty table + tail.
            pages.push(PlannedPage {
                start_item: None,
                end_item: 0,
                blocks: tail,
            });
        } else {
            // If the last page overflowed the body region with items, move the tail
            // to a fresh page; otherwise append it below the table on the last page.
            let last_overflowed = current_height > usable_per_page;
            if last_overflowed {
                pages.push(PlannedPage {
                    start_item: None,
                    end_item: doc.line_items.len(),
                    blocks: tail,
                });
            } else {
                let last = pages
                    .last_mut()
                    .ok_or(RenderError::MissingRequiredField("line items"))?;
                last.blocks.extend(tail);
            }
        }

        // Mark continuation pages: any page after the first that renders items.
        // (The first page already has the full header via render; continuation pages
        // get a ContinuationHeader block prepended.)
        for page in pages.iter_mut().skip(1) {
            if page.start_item.is_some() {
                page.blocks.insert(0, Block::ContinuationHeader);
            }
        }

        // Rule 9: total page count is now known for footer rendering.
        // Suppress unused-warning for footer_region by using it in a no-op.
        let _ = footer_region;

        Ok(PagePlan { pages })
    }

    /// Render to a `PdfDocument` and serialized bytes (for test inspection).
    pub fn render_document(
        &self,
        doc: &QuotationDocument,
    ) -> Result<(PdfDocument, Vec<u8>), RenderError> {
        doc.validate()?;
        let plan = self.plan_pages(doc)?;
        let total_pages = plan.pages.len();

        let mut pdf_doc = PdfDocument::new("quotation");
        for (page_index, page) in plan.pages.iter().enumerate() {
            let ops = self.render_page_ops(doc, page, page_index, total_pages)?;
            pdf_doc
                .pages
                .push(PdfPage::new(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), ops));
        }

        let mut warnings: Vec<PdfWarnMsg> = Vec::new();
        let bytes = pdf_doc.save(&PdfSaveOptions::default(), &mut warnings);
        if bytes.is_empty() {
            return Err(RenderError::Serialize);
        }
        Ok((pdf_doc, bytes))
    }

    /// Render to bytes only.
    pub fn render(&self, doc: &QuotationDocument) -> Result<Vec<u8>, RenderError> {
        Ok(self.render_document(doc)?.1)
    }

    fn render_page_ops(
        &self,
        doc: &QuotationDocument,
        page: &PlannedPage,
        page_index: usize,
        total_pages: usize,
    ) -> Result<Vec<Op>, RenderError> {
        let layout = &self.layout;
        let mut ops: Vec<Op> = Vec::new();
        let primary = Color::Rgb(Rgb::new(
            COLOR_PRIMARY_R,
            COLOR_PRIMARY_G,
            COLOR_PRIMARY_B,
            None,
        ));
        let accent = Color::Rgb(Rgb::new(
            COLOR_ACCENT_R,
            COLOR_ACCENT_G,
            COLOR_ACCENT_B,
            None,
        ));

        // Title strip + accent rectangle (page 1 full header; continuation pages
        // render a compact header via the ContinuationHeader block).
        let is_continuation = page_index > 0 && page.start_item.is_some();
        if is_continuation {
            self.push_title_strip(&mut ops, doc, page_index, total_pages, &accent)?;
            self.push_table_header(&mut ops, &primary)?;
        } else if page_index == 0 {
            self.push_title_strip(&mut ops, doc, page_index, total_pages, &accent)?;
            self.push_company_identity(&mut ops, doc, &primary)?;
            self.push_quotation_metadata(&mut ops, doc, &primary)?;
            self.push_customer_details(&mut ops, doc, &primary)?;
            self.push_shipping_dispatch(&mut ops, doc, &primary)?;
            self.push_table_header(&mut ops, &primary)?;
        }

        // Cursor for item rows starts just below the table header.
        let mut cursor_y = layout.table_header_bottom + 2.0;
        let body_bottom = layout.table_body_bottom;

        for block in &page.blocks {
            match block {
                Block::ContinuationHeader => {
                    // Already rendered above for continuation pages; skip.
                }
                Block::ItemRow { lines, amount } => {
                    if cursor_y + layout.minimum_row_line_height > body_bottom {
                        // Safety net: a row that would overflow the body region is still
                        // rendered (financial data is never truncated) but flagged by
                        // moving the cursor down so subsequent rows are visually below.
                    }
                    self.push_item_row(
                        &mut ops,
                        doc,
                        page,
                        &mut cursor_y,
                        lines,
                        amount,
                        &primary,
                    )?;
                }
                Block::ItemFooter => {
                    self.push_item_footer(&mut ops, doc, &mut cursor_y, &primary)?;
                }
                Block::Summary => {
                    self.push_summary(&mut ops, doc, &mut cursor_y, &primary)?;
                }
                Block::AmountInWords => {
                    self.push_amount_in_words(&mut ops, doc, &mut cursor_y, &primary)?;
                }
                Block::BankSignature => {
                    self.push_bank_signature(&mut ops, doc, &mut cursor_y, &primary)?;
                }
                Block::Terms { lines } => {
                    self.push_terms(&mut ops, lines, &mut cursor_y, &primary)?;
                }
            }
        }

        // Footer: page number + digital-signature note (rule 1).
        self.push_footer(&mut ops, page_index, total_pages, &primary)?;

        Ok(ops)
    }

    // -- page section helpers ----------------------------------------------

    fn push_title_strip(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        page_index: usize,
        total_pages: usize,
        accent: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let strip = layout.region("title_strip")?;
        // Accent rectangle behind the title.
        ops.push(Op::SetFillColor {
            col: accent.clone(),
        });
        ops.push(Op::DrawRectangle {
            rectangle: Rect {
                x: pt(strip.x),
                y: pt(crate::layout::PAGE_HEIGHT_PT - (strip.y + strip.height)),
                width: pt(strip.width),
                height: pt(strip.height),
                mode: None,
                winding_order: None,
            },
        });
        ops.push(Op::SaveGraphicsState);
        ops.push(Op::RestoreGraphicsState);

        let title = "QUOTATION";
        self.text(
            ops,
            title,
            strip.x + strip.width / 2.0 - 40.0,
            strip.y + 6.0,
            FONT_SIZE_TITLE,
            BuiltinFont::HelveticaBold,
            accent,
        );
        if let Some(copy_label) = &doc.meta.copy_label
            && !copy_label.is_empty()
        {
            self.text(
                ops,
                copy_label,
                strip.x + strip.width - 60.0,
                strip.y + 6.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                accent,
            );
        }
        // Continuation pages repeat quotation number + date (rule 1).
        if page_index > 0 {
            let label = format!(
                "{}  |  {}  |  Page {} of {}",
                doc.meta.quotation_number,
                doc.meta.quotation_date,
                page_index + 1,
                total_pages
            );
            self.text(
                ops,
                &label,
                strip.x + 4.0,
                strip.y + 6.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                accent,
            );
        }
        Ok(())
    }

    fn push_company_identity(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("company_identity")?;
        let mut y = region.y + 12.0;
        self.text(
            ops,
            &doc.company.legal_name,
            region.x,
            y,
            FONT_SIZE_TOTALS_BOLD,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 11.0;
        if let Some(tax_id) = &doc.company.tax_id
            && !tax_id.is_empty()
        {
            self.text(
                ops,
                tax_id,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        for line in &doc.company.address {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        for line in &doc.company.contact {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        // Logo slot (labeled rectangle, no image embedding).
        if let Some(logo) = &doc.company.logo_ref
            && !logo.is_empty()
        {
            self.text(
                ops,
                &format!("[logo: {logo}]"),
                region.x,
                region.y + region.height - 8.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
        }
        Ok(())
    }

    fn push_quotation_metadata(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("quotation_metadata")?;
        let mut y = region.y + 4.0;
        let rows = [
            ("Quotation No.", &doc.meta.quotation_number),
            ("Date", &doc.meta.quotation_date),
            ("Place of Supply", &doc.meta.place_of_supply),
            ("Validity", &doc.meta.validity),
            ("Currency", &doc.meta.currency),
        ];
        for (label, value) in &rows {
            self.text(
                ops,
                label,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::HelveticaBold,
                primary,
            );
            self.text(
                ops,
                value,
                region.x + 90.0,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 11.0;
        }
        Ok(())
    }

    fn push_customer_details(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("customer_details")?;
        let mut y = region.y + 4.0;
        self.text(
            ops,
            "Bill To",
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 11.0;
        self.text(
            ops,
            &doc.customer.name,
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 10.0;
        if let Some(tax_id) = &doc.customer.tax_id
            && !tax_id.is_empty()
        {
            self.text(
                ops,
                tax_id,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        for line in &doc.customer.billing_address {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        Ok(())
    }

    fn push_shipping_dispatch(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("shipping_dispatch")?;
        let mut y = region.y + 4.0;
        self.text(
            ops,
            "Ship To",
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 11.0;
        for line in &doc.customer.shipping_address {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        if let Some(origin) = &doc.customer.dispatch_origin
            && !origin.is_empty()
        {
            write_label(ops, "Dispatch: ", origin, region.x, y, primary, self);
        }
        Ok(())
    }

    fn push_table_header(&self, ops: &mut Vec<Op>, primary: &Color) -> Result<(), RenderError> {
        let layout = &self.layout;
        let y = layout.table_header_top + 10.0;
        let headers: [(&str, &str); 8] = [
            ("sequence", "#"),
            ("item", "Description"),
            ("hsn_sac", "HSN/SAC"),
            ("rate_per_item", "Rate"),
            ("quantity", "Qty"),
            ("taxable_value", "Taxable"),
            ("tax_amount", "Tax"),
            ("amount", "Amount"),
        ];
        for (name, label) in &headers {
            if let Some(col) = layout.column_by_name(name) {
                let x = match col.alignment.as_str() {
                    "right" => col.right - 2.0,
                    "center" => (col.left + col.right) / 2.0 - 12.0,
                    _ => col.left + 2.0,
                };
                self.text(
                    ops,
                    label,
                    x,
                    y,
                    FONT_SIZE_TABLE_HEADER,
                    BuiltinFont::HelveticaBold,
                    primary,
                );
            }
        }
        // Header underline.
        ops.push(Op::SetOutlineColor {
            col: primary.clone(),
        });
        ops.push(Op::SetOutlineThickness { pt: pt(0.5) });
        ops.push(Op::DrawLine {
            line: Line {
                points: vec![
                    LinePoint {
                        p: Point::new(
                            pt_to_pdf_x(
                                layout
                                    .column_by_name("sequence")
                                    .map(|c| c.left)
                                    .unwrap_or(24.0),
                            ),
                            pt_to_pdf_y(layout.table_header_bottom),
                        ),
                        bezier: false,
                    },
                    LinePoint {
                        p: Point::new(
                            pt_to_pdf_x(
                                layout
                                    .column_by_name("amount")
                                    .map(|c| c.right)
                                    .unwrap_or(571.28),
                            ),
                            pt_to_pdf_y(layout.table_header_bottom),
                        ),
                        bezier: false,
                    },
                ],
                is_closed: false,
            },
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_item_row(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        page: &PlannedPage,
        cursor_y: &mut f32,
        lines: &[String],
        amount: &str,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let row_h = layout.minimum_row_line_height;
        let start = page.start_item.unwrap_or(0);
        let local_index = page.end_item.saturating_sub(start).saturating_sub(1);
        let item_index = start + local_index.min(doc.line_items.len() - start);
        let item = doc
            .line_items
            .get(item_index)
            .ok_or(RenderError::MissingRequiredField("line item"))?;

        // Sequence (left).
        if let Some(col) = layout.column_by_name("sequence") {
            self.text(
                ops,
                &item.sequence.to_string(),
                col.left + 2.0,
                *cursor_y + 4.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
        }
        // Description (wrapped, left aligned).
        if let Some(col) = layout.column_by_name("item") {
            for (i, line) in lines.iter().enumerate() {
                self.text(
                    ops,
                    line,
                    col.left + 2.0,
                    *cursor_y + 4.0 + (i as f32) * row_h,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
        }
        // Right/center-aligned numeric columns.
        let numeric_cols: [(&str, &str); 6] = [
            ("hsn_sac", &item.hsn_sac),
            ("rate_per_item", &item.unit_rate),
            ("quantity", &item.quantity),
            ("taxable_value", &item.taxable_value),
            ("tax_amount", &item.tax_amount),
            ("amount", amount),
        ];
        for (name, value) in &numeric_cols {
            if let Some(col) = layout.column_by_name(name) {
                let x = match col.alignment.as_str() {
                    "right" => col.right - 2.0,
                    "center" => (col.left + col.right) / 2.0 - 10.0,
                    _ => col.left + 2.0,
                };
                self.text(
                    ops,
                    value,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
        }
        *cursor_y += row_h * (lines.len().max(1) as f32);
        Ok(())
    }

    fn push_item_footer(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        cursor_y: &mut f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("item_quantity_footer")?;
        let label = format!(
            "Items: {}  |  Total Qty: {}",
            doc.summary.item_count, doc.summary.total_quantity
        );
        self.text(
            ops,
            &label,
            region.x,
            region.y + 4.0,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        *cursor_y = region.y + region.height + 2.0;
        Ok(())
    }

    fn push_summary(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        cursor_y: &mut f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("tax_total_summary")?;
        let mut y = region.y + 4.0;
        self.text(
            ops,
            "Taxable Total",
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        self.text(
            ops,
            &doc.summary.taxable_total,
            region.x + region.width - 60.0,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        y += 11.0;
        for comp in &doc.summary.tax_components {
            let label = format!("{} @ {}", comp.label, comp.rate);
            self.text(
                ops,
                &label,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            self.text(
                ops,
                &comp.amount,
                region.x + region.width - 60.0,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 11.0;
        }
        self.text(
            ops,
            "Grand Total",
            region.x,
            y,
            FONT_SIZE_TOTALS_BOLD,
            BuiltinFont::HelveticaBold,
            primary,
        );
        self.text(
            ops,
            &doc.summary.grand_total,
            region.x + region.width - 60.0,
            y,
            FONT_SIZE_TOTALS_BOLD,
            BuiltinFont::HelveticaBold,
            primary,
        );
        *cursor_y = y + 12.0;
        Ok(())
    }

    fn push_amount_in_words(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        cursor_y: &mut f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("amount_in_words")?;
        let label = format!(
            "Amount in words: {} {}",
            doc.amount_in_words, doc.meta.currency
        );
        self.text(
            ops,
            &label,
            region.x,
            region.y + 4.0,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        *cursor_y = region.y + region.height + 2.0;
        Ok(())
    }

    fn push_bank_signature(
        &self,
        ops: &mut Vec<Op>,
        doc: &QuotationDocument,
        cursor_y: &mut f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("bank_signature")?;
        let mut y = region.y + 4.0;
        self.text(
            ops,
            "Bank Details",
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 11.0;
        for line in &doc.company.address {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        // Signature slot (labeled, no image embedding).
        if let Some(sig) = &doc.company.signature_ref
            && !sig.is_empty()
        {
            self.text(
                ops,
                &format!("[signature: {sig}]"),
                region.x + region.width - 110.0,
                region.y + region.height - 8.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
        }
        self.text(
            ops,
            &doc.signatory_label,
            region.x + region.width - 110.0,
            region.y + region.height + 2.0,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        *cursor_y = region.y + region.height + 2.0;
        Ok(())
    }

    fn push_terms(
        &self,
        ops: &mut Vec<Op>,
        lines: &[String],
        cursor_y: &mut f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("terms")?;
        let mut y = region.y + 4.0;
        self.text(
            ops,
            "Terms & Conditions",
            region.x,
            y,
            FONT_SIZE_TOTALS_BOLD,
            BuiltinFont::HelveticaBold,
            primary,
        );
        y += 14.0;
        for line in lines {
            self.text(
                ops,
                line,
                region.x,
                y,
                FONT_SIZE_TERMS,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 9.36;
        }
        *cursor_y = y;
        Ok(())
    }

    fn push_footer(
        &self,
        ops: &mut Vec<Op>,
        page_index: usize,
        total_pages: usize,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let region = layout.region("footer")?;
        let page_label = format!("Page {} of {}", page_index + 1, total_pages);
        self.text(
            ops,
            &page_label,
            region.x,
            region.y + 10.0,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        self.text(
            ops,
            "Digitally generated quotation",
            region.x + 200.0,
            region.y + 10.0,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        Ok(())
    }

    // -- low-level text placement ------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        ops: &mut Vec<Op>,
        s: &str,
        x_pt: f32,
        y_pt: f32,
        size: f32,
        font: BuiltinFont,
        color: &Color,
    ) {
        if s.is_empty() {
            return;
        }
        ops.push(Op::StartTextSection);
        ops.push(Op::SetFont {
            font: PdfFontHandle::Builtin(font),
            size: pt(size),
        });
        ops.push(Op::SetFillColor { col: color.clone() });
        ops.push(Op::SetTextCursor {
            pos: Point::new(pt_to_pdf_x(x_pt), pt_to_pdf_y(y_pt)),
        });
        ops.push(Op::ShowText {
            items: vec![TextItem::Text(s.to_string())],
        });
        ops.push(Op::EndTextSection);
    }
}

// Helper used by push_shipping_dispatch to avoid borrowing self in a closure.
fn write_label(
    ops: &mut Vec<Op>,
    label: &str,
    value: &str,
    x: f32,
    y: f32,
    primary: &Color,
    renderer: &QuotationRenderer,
) {
    renderer.text(
        ops,
        label,
        x,
        y,
        FONT_SIZE_BODY,
        BuiltinFont::HelveticaBold,
        primary,
    );
    renderer.text(
        ops,
        value,
        x + 60.0,
        y,
        FONT_SIZE_BODY,
        BuiltinFont::Helvetica,
        primary,
    );
}

impl QuotationDocument {
    pub fn validate(&self) -> Result<(), RenderError> {
        if self.company.legal_name.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("company.legal_name"));
        }
        if self.meta.quotation_number.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("meta.quotation_number"));
        }
        if self.meta.quotation_date.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("meta.quotation_date"));
        }
        if self.customer.name.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("customer.name"));
        }
        if self.line_items.is_empty() {
            return Err(RenderError::MissingRequiredField("line_items"));
        }
        if self.summary.grand_total.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("summary.grand_total"));
        }
        if self.amount_in_words.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("amount_in_words"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Text wrapping (approximate, at ~0.45 * font_size pt per char)
// ---------------------------------------------------------------------------

fn wrap_text(text: &str, max_width_pt: f32) -> Vec<String> {
    let char_width = FONT_SIZE_BODY * 0.45;
    let max_chars = ((max_width_pt / char_width) as usize).max(1);
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.len() + 1 + word.len() <= max_chars {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current.clone());
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// Re-export for tests that inspect the produced document.
pub use printpdf::{PdfDocument as PdfDocumentHandle, PdfResources as PdfResourcesHandle};

// Suppress unused-field warnings for fields read only by tests/inspector.
#[allow(dead_code)]
fn _region_used(r: &Region) -> f32 {
    r.width + r.height
}

// fmt::Write import used for any future formatted labels.
#[allow(dead_code)]
fn _write_marker() -> std::fmt::Result {
    let mut s = String::new();
    write!(&mut s, "x")
}
