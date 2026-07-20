//! Quotation PDF rendering using the approved layout contract.

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
    FONT_SIZE_TOTALS_BOLD, PAGE_HEIGHT_MM, PAGE_WIDTH_MM, QuotationLayout, pt, pt_to_pdf_x,
    pt_to_pdf_y,
};

// Approximate average character width for Helvetica at a given size (in pt).
// Helvetica's average glyph advance is ~0.5em.
fn text_width_pt(s: &str, size: f32) -> f32 {
    s.chars().count() as f32 * size * 0.5
}

// ---------------------------------------------------------------------------
// Render data model (caller-supplied, pre-formatted strings)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuotationDocument {
    pub company: CompanyRenderData,
    pub bank: BankRenderData,
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
pub struct BankRenderData {
    pub bank_name: String,
    pub account_name: String,
    pub account_number: String,
    pub ifsc: String,
    pub branch: Option<String>,
    pub upi_id: Option<String>,
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
    /// Page header: title strip + quotation number/date + (optionally) table header.
    /// Rendered on every page (rule 1). `with_table_header` is true on pages that
    /// contain item rows.
    PageHeader { with_table_header: bool },
    /// A fragment of a line item. `is_continuation` fragments repeat sequence/HSN
    /// context but do NOT repeat financial amounts (rule 4). Only the final
    /// fragment (`is_continuation == false`) renders financial columns.
    ItemRow {
        item_index: usize,
        lines: Vec<String>,
        is_continuation: bool,
    },
    /// Item/quantity footer.
    ItemFooter,
    /// Tax + total summary block (kept intact; rule 8).
    Summary,
    /// Amount in words (one line; rule 8).
    AmountInWords,
    /// Bank + signature panel (kept intact; rule 6).
    BankSignature,
    /// A chunk of terms lines (terms paginate across pages; rule 7).
    Terms { lines: Vec<String> },
}

impl Block {
    /// Approximate height in points consumed by this block.
    #[allow(dead_code)]
    fn height_pt(&self, layout: &QuotationLayout) -> f32 {
        let row_h = layout.minimum_row_line_height;
        match self {
            Self::PageHeader { with_table_header } => {
                let header = layout.table_header_bottom - layout.table_header_top;
                24.0 + if *with_table_header {
                    header + 6.0
                } else {
                    0.0
                }
            }
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
        let terms_region = layout.region("terms")?;
        let terms_height = terms_region.height;
        let footer_region = layout.region("footer")?;

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

        // Page 1 starts with a full header (title strip + company + metadata + table header).
        // Continuation pages start with a compact header (title strip + table header).
        // The header is added per-page below; here we only track item/tail blocks.

        for (idx, item_lines) in wrapped.iter().enumerate() {
            let row_h = layout.minimum_row_line_height;
            let full_height = row_h * (item_lines.len() as f32);

            // Rule 4: if a single item exceeds a full page, split it at line boundaries.
            // Each fragment consumes one page's worth of lines. Financial amounts appear
            // only on the final fragment.
            let max_lines_per_page = (usable_per_page / row_h).floor() as usize;
            if max_lines_per_page == 0 {
                // Degenerate layout: render the item as a single block regardless.
                let block = Block::ItemRow {
                    item_index: idx,
                    lines: item_lines.clone(),
                    is_continuation: false,
                };
                current_blocks.push(block);
                current_height += full_height;
                continue;
            }

            if full_height > usable_per_page {
                // Flush current page first.
                if !current_blocks.is_empty() {
                    pages.push(PlannedPage {
                        blocks: std::mem::take(&mut current_blocks),
                    });
                    current_height = 0.0;
                }
                // Split the item across pages.
                let chunks: Vec<Vec<String>> = item_lines
                    .chunks(max_lines_per_page)
                    .map(|c| c.to_vec())
                    .collect();
                for (frag_idx, chunk) in chunks.iter().enumerate() {
                    let is_continuation = frag_idx < chunks.len() - 1;
                    if !current_blocks.is_empty()
                        && current_height + row_h * (chunk.len() as f32) > usable_per_page
                    {
                        pages.push(PlannedPage {
                            blocks: std::mem::take(&mut current_blocks),
                        });
                        current_height = 0.0;
                    }
                    current_blocks.push(Block::ItemRow {
                        item_index: idx,
                        lines: chunk.clone(),
                        is_continuation,
                    });
                    current_height += row_h * (chunk.len() as f32);
                }
                continue;
            }

            // Normal item: flush the page if the next row would overflow.
            if current_height + full_height > usable_per_page && !current_blocks.is_empty() {
                pages.push(PlannedPage {
                    blocks: std::mem::take(&mut current_blocks),
                });
                current_height = 0.0;
            }
            current_blocks.push(Block::ItemRow {
                item_index: idx,
                lines: item_lines.clone(),
                is_continuation: false,
            });
            current_height += full_height;
        }

        // Flush remaining item blocks before handling the financial summary tail.
        if !current_blocks.is_empty() {
            pages.push(PlannedPage {
                blocks: std::mem::take(&mut current_blocks),
            });
        }

        // Financial summary tail: item footer + summary + amount-in-words + bank + terms.
        // Rule 5/6/8: keep summary, amount-in-words, bank/signature intact; move as a unit.
        let terms_width = terms_region.width - 4.0;
        let terms_lines: Vec<String> = doc
            .terms
            .iter()
            .flat_map(|t| wrap_text(t, terms_width))
            .collect();
        // Rule 7: terms paginate. Split terms lines into chunks that fit the terms region.
        let terms_line_h = 9.36_f32;
        let terms_heading_h = 18.0;
        let max_terms_lines =
            (((terms_height - terms_heading_h) / terms_line_h).floor() as usize).max(1);
        let terms_chunks: Vec<Vec<String>> = if terms_lines.is_empty() {
            vec![Vec::new()]
        } else {
            terms_lines
                .chunks(max_terms_lines)
                .map(|c| c.to_vec())
                .collect()
        };

        let mut tail: Vec<Block> = vec![
            Block::ItemFooter,
            Block::Summary,
            Block::AmountInWords,
            Block::BankSignature,
        ];
        // First terms chunk goes in the tail; additional chunks become their own pages.
        if let Some(first_terms) = terms_chunks.first() {
            tail.push(Block::Terms {
                lines: first_terms.clone(),
            });
        }

        if pages.is_empty() {
            // No items: a single page holds the empty table + tail.
            pages.push(PlannedPage { blocks: tail });
        } else {
            // If the last page overflowed the body region with items, move the tail
            // to a fresh page; otherwise append it below the table on the last page.
            let last_overflowed = current_height > usable_per_page;
            if last_overflowed {
                pages.push(PlannedPage { blocks: tail });
            } else {
                let last = pages
                    .last_mut()
                    .ok_or(RenderError::MissingRequiredField("line items"))?;
                last.blocks.extend(tail);
            }
        }

        // Additional terms chunks go on their own continuation pages (rule 7).
        for extra_terms in terms_chunks.iter().skip(1) {
            pages.push(PlannedPage {
                blocks: vec![Block::Terms {
                    lines: extra_terms.clone(),
                }],
            });
        }

        // Prepend a PageHeader to every page (rule 1: every page repeats the title
        // strip + quotation number/date). Pages with item rows also get the table header.
        for page in pages.iter_mut() {
            let has_items = page
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ItemRow { .. }));
            page.blocks.insert(
                0,
                Block::PageHeader {
                    with_table_header: has_items,
                },
            );
        }

        // Rule 9: total page count is now known for footer rendering.
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
        // Title-strip text is white on the accent-filled strip for contrast.
        let strip_text = Color::Rgb(Rgb::new(1.0, 1.0, 1.0, None));

        let mut cursor_y = layout.table_header_bottom + 2.0;
        let body_bottom = layout.table_body_bottom;

        for block in &page.blocks {
            match block {
                Block::PageHeader { with_table_header } => {
                    self.push_title_strip(
                        &mut ops,
                        doc,
                        page_index,
                        total_pages,
                        &accent,
                        &strip_text,
                    )?;
                    if page_index == 0 {
                        self.push_company_identity(&mut ops, doc, &primary)?;
                        self.push_quotation_metadata(&mut ops, doc, &primary)?;
                        self.push_customer_details(&mut ops, doc, &primary)?;
                        self.push_shipping_dispatch(&mut ops, doc, &primary)?;
                    }
                    if *with_table_header {
                        // Reset cursor below the table header.
                        cursor_y = layout.table_header_bottom + 2.0;
                        self.push_table_header(&mut ops, &primary)?;
                    }
                }
                Block::ItemRow {
                    item_index,
                    lines,
                    is_continuation,
                } => {
                    let item = doc
                        .line_items
                        .get(*item_index)
                        .ok_or(RenderError::MissingRequiredField("line item"))?;
                    self.push_item_row(
                        &mut ops,
                        item,
                        lines,
                        *is_continuation,
                        &mut cursor_y,
                        body_bottom,
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
        text_color: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let strip = layout.region("title_strip")?;
        ops.push(Op::SetFillColor {
            col: accent.clone(),
        });
        ops.push(Op::DrawRectangle {
            rectangle: Rect {
                x: pt(strip.x),
                y: pt(PAGE_HEIGHT_PT - (strip.y + strip.height)),
                width: pt(strip.width),
                height: pt(strip.height),
                mode: None,
                winding_order: None,
            },
        });
        ops.push(Op::SaveGraphicsState);
        ops.push(Op::RestoreGraphicsState);

        let title = "QUOTATION";
        let title_w = text_width_pt(title, FONT_SIZE_TITLE);
        self.text(
            ops,
            title,
            strip.x + (strip.width - title_w) / 2.0,
            strip.y + 6.0,
            FONT_SIZE_TITLE,
            BuiltinFont::HelveticaBold,
            text_color,
        );
        if let Some(copy_label) = &doc.meta.copy_label
            && !copy_label.is_empty()
        {
            let label_w = text_width_pt(copy_label, FONT_SIZE_BODY);
            self.text(
                ops,
                copy_label,
                strip.x + strip.width - label_w - 4.0,
                strip.y + 6.0,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                text_color,
            );
        }
        // Every page repeats quotation number + date (rule 1).
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
            text_color,
        );
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
            self.text(
                ops,
                "Dispatch:",
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::HelveticaBold,
                primary,
            );
            self.text(
                ops,
                origin,
                region.x + 60.0,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
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
                let x = self.align_x(col, label, FONT_SIZE_TABLE_HEADER);
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
        ops.push(Op::SetOutlineColor {
            col: primary.clone(),
        });
        ops.push(Op::SetOutlineThickness { pt: pt(0.5) });
        let left_x = layout
            .column_by_name("sequence")
            .map(|c| c.left)
            .unwrap_or(24.0);
        let right_x = layout
            .column_by_name("amount")
            .map(|c| c.right)
            .unwrap_or(571.28);
        ops.push(Op::DrawLine {
            line: Line {
                points: vec![
                    LinePoint {
                        p: Point::new(pt_to_pdf_x(left_x), pt_to_pdf_y(layout.table_header_bottom)),
                        bezier: false,
                    },
                    LinePoint {
                        p: Point::new(
                            pt_to_pdf_x(right_x),
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
        item: &LineItemRenderData,
        lines: &[String],
        is_continuation: bool,
        cursor_y: &mut f32,
        _body_bottom: f32,
        primary: &Color,
    ) -> Result<(), RenderError> {
        let layout = &self.layout;
        let row_h = layout.minimum_row_line_height;

        // Sequence (left) — repeated on continuation fragments for context (rule 4).
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
        // Financial columns only on the final fragment (rule 4).
        if !is_continuation {
            // HSN/SAC (center).
            if let Some(col) = layout.column_by_name("hsn_sac") {
                let x = self.align_x(col, &item.hsn_sac, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &item.hsn_sac,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
            // Rate per item (right).
            if let Some(col) = layout.column_by_name("rate_per_item") {
                let x = self.align_x(col, &item.unit_rate, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &item.unit_rate,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
            // Quantity + unit (right) — layout contract: "Quantity and unit".
            if let Some(col) = layout.column_by_name("quantity") {
                let qty = format!("{} {}", item.quantity, item.unit);
                let x = self.align_x(col, &qty, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &qty,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
            // Taxable value (right).
            if let Some(col) = layout.column_by_name("taxable_value") {
                let x = self.align_x(col, &item.taxable_value, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &item.taxable_value,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
            // Tax amount + rate (right) — layout contract: "Tax amount and rate".
            if let Some(col) = layout.column_by_name("tax_amount") {
                let tax = format!("{} ({})", item.tax_amount, item.tax_rate);
                let x = self.align_x(col, &tax, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &tax,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::Helvetica,
                    primary,
                );
            }
            // Amount (right).
            if let Some(col) = layout.column_by_name("amount") {
                let x = self.align_x(col, &item.amount, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &item.amount,
                    x,
                    *cursor_y + 4.0,
                    FONT_SIZE_BODY,
                    BuiltinFont::HelveticaBold,
                    primary,
                );
            }
        } else {
            // Continuation fragment: repeat HSN/SAC context, no financial amounts (rule 4).
            if let Some(col) = layout.column_by_name("hsn_sac") {
                let x = self.align_x(col, &item.hsn_sac, FONT_SIZE_BODY);
                self.text(
                    ops,
                    &item.hsn_sac,
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

    /// Compute the x position for text in a column based on alignment.
    /// Right-aligned text is placed so its right edge sits 2pt inside the column.
    fn align_x(&self, col: &crate::layout::TableColumn, text: &str, size: f32) -> f32 {
        match col.alignment.as_str() {
            "right" => col.right - text_width_pt(text, size) - 2.0,
            "center" => col.left + (col.width() - text_width_pt(text, size)) / 2.0,
            _ => col.left + 2.0,
        }
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
        self.text_right(
            ops,
            &doc.summary.taxable_total,
            region.x + region.width,
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
            self.text_right(
                ops,
                &comp.amount,
                region.x + region.width,
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
        self.text_right(
            ops,
            &doc.summary.grand_total,
            region.x + region.width,
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
        self.text(
            ops,
            &doc.bank.bank_name,
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        y += 10.0;
        self.text(
            ops,
            &doc.bank.account_name,
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        y += 10.0;
        self.text(
            ops,
            &doc.bank.account_number,
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        y += 10.0;
        self.text(
            ops,
            &doc.bank.ifsc,
            region.x,
            y,
            FONT_SIZE_BODY,
            BuiltinFont::Helvetica,
            primary,
        );
        y += 10.0;
        if let Some(branch) = &doc.bank.branch
            && !branch.is_empty()
        {
            self.text(
                ops,
                branch,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
            y += 10.0;
        }
        if let Some(upi) = &doc.bank.upi_id
            && !upi.is_empty()
        {
            self.text(
                ops,
                upi,
                region.x,
                y,
                FONT_SIZE_BODY,
                BuiltinFont::Helvetica,
                primary,
            );
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

    /// Right-aligned text: the right edge of the text sits at `right_x_pt`.
    #[allow(clippy::too_many_arguments)]
    fn text_right(
        &self,
        ops: &mut Vec<Op>,
        s: &str,
        right_x_pt: f32,
        y_pt: f32,
        size: f32,
        font: BuiltinFont,
        color: &Color,
    ) {
        let w = text_width_pt(s, size);
        self.text(ops, s, right_x_pt - w, y_pt, size, font, color);
    }
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
        if self.bank.bank_name.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("bank.bank_name"));
        }
        if self.bank.account_number.trim().is_empty() {
            return Err(RenderError::MissingRequiredField("bank.account_number"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Text wrapping (approximate, at ~0.5 * font_size pt per char)
// ---------------------------------------------------------------------------

fn wrap_text(text: &str, max_width_pt: f32) -> Vec<String> {
    let char_width = FONT_SIZE_BODY * 0.5;
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

/// Re-exported for tests that inspect the produced document.
pub use printpdf::PdfDocument as PdfDocumentHandle;

use crate::layout::PAGE_HEIGHT_PT;
