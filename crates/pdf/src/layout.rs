use printpdf::units::{Mm, Pt};
use serde::Deserialize;
use std::collections::HashMap;

// Version 1 uses PDF standard Helvetica as the approved-by-prototype substitute
// for the reference's SFUIDisplay/SFUIText subsets; embedded-font substitution
// is a separate visual-overlay approval item.

pub const PAGE_WIDTH_PT: f32 = 595.28;
pub const PAGE_HEIGHT_PT: f32 = 841.89;
pub const PAGE_WIDTH_MM: f32 = 210.0;
pub const PAGE_HEIGHT_MM: f32 = 297.0;

pub const FONT_SIZE_TITLE: f32 = 12.0;
pub const FONT_SIZE_TABLE_HEADER: f32 = 8.5;
pub const FONT_SIZE_BODY: f32 = 8.0;
pub const FONT_SIZE_TOTALS_BOLD: f32 = 9.0;
pub const FONT_SIZE_TERMS: f32 = 8.0;

pub const COLOR_PRIMARY_R: f32 = 0.114;
pub const COLOR_PRIMARY_G: f32 = 0.114;
pub const COLOR_PRIMARY_B: f32 = 0.122;
pub const COLOR_ACCENT_R: f32 = 0.153;
pub const COLOR_ACCENT_G: f32 = 0.431;
pub const COLOR_ACCENT_B: f32 = 0.945;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TableColumn {
    pub name: String,
    pub left: f32,
    pub right: f32,
    pub alignment: String,
}

impl TableColumn {
    pub fn width(&self) -> f32 {
        self.right - self.left
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LayoutError {
    #[error("invalid layout JSON")]
    InvalidJson,
    #[error("missing region: {0}")]
    MissingRegion(&'static str),
}

#[derive(Debug, Clone, Deserialize)]
struct LayoutFileJson {
    coordinates: CoordinatesJson,
}

#[derive(Debug, Clone, Deserialize)]
struct CoordinatesJson {
    page: PageJson,
    regions: RegionsJson,
    table: TableDefJson,
}

#[derive(Debug, Clone, Deserialize)]
struct PageJson {
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct RegionsJson {
    title_strip: RegionDefJson,
    company_identity: RegionDefJson,
    quotation_metadata: RegionDefJson,
    customer_details: RegionDefJson,
    shipping_dispatch: RegionDefJson,
    line_item_table: RegionDefJson,
    item_quantity_footer: RegionDefJson,
    tax_total_summary: RegionDefJson,
    amount_in_words: RegionDefJson,
    bank_signature: RegionDefJson,
    terms: RegionDefJson,
    footer: RegionDefJson,
}

#[derive(Debug, Clone, Deserialize)]
struct RegionDefJson {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct TableDefJson {
    header_top: f32,
    header_bottom: f32,
    body_bottom: f32,
    text_inset: f32,
    minimum_row_line_height: f32,
    columns: Vec<ColumnDefJson>,
}

#[derive(Debug, Clone, Deserialize)]
struct ColumnDefJson {
    name: String,
    left: f32,
    right: f32,
    alignment: String,
}

#[derive(Debug, Clone)]
pub struct QuotationLayout {
    pub page_width: f32,
    pub page_height: f32,
    pub regions: HashMap<&'static str, Region>,
    pub columns: Vec<TableColumn>,
    pub table_header_top: f32,
    pub table_header_bottom: f32,
    pub table_body_bottom: f32,
    pub table_text_inset: f32,
    pub minimum_row_line_height: f32,
}

impl QuotationLayout {
    pub fn from_fixture_json(json: &str) -> Result<Self, LayoutError> {
        let parsed: LayoutFileJson =
            serde_json::from_str(json).map_err(|_| LayoutError::InvalidJson)?;
        let coords = parsed.coordinates;

        let mut regions: HashMap<&'static str, Region> = HashMap::new();

        let insert_region = |regions: &mut HashMap<&'static str, Region>,
                             name: &'static str,
                             def: &RegionDefJson| {
            regions.insert(
                name,
                Region {
                    x: def.x,
                    y: def.y,
                    width: def.width,
                    height: def.height,
                },
            );
        };

        insert_region(&mut regions, "title_strip", &coords.regions.title_strip);
        insert_region(
            &mut regions,
            "company_identity",
            &coords.regions.company_identity,
        );
        insert_region(
            &mut regions,
            "quotation_metadata",
            &coords.regions.quotation_metadata,
        );
        insert_region(
            &mut regions,
            "customer_details",
            &coords.regions.customer_details,
        );
        insert_region(
            &mut regions,
            "shipping_dispatch",
            &coords.regions.shipping_dispatch,
        );
        insert_region(
            &mut regions,
            "line_item_table",
            &coords.regions.line_item_table,
        );
        insert_region(
            &mut regions,
            "item_quantity_footer",
            &coords.regions.item_quantity_footer,
        );
        insert_region(
            &mut regions,
            "tax_total_summary",
            &coords.regions.tax_total_summary,
        );
        insert_region(
            &mut regions,
            "amount_in_words",
            &coords.regions.amount_in_words,
        );
        insert_region(
            &mut regions,
            "bank_signature",
            &coords.regions.bank_signature,
        );
        insert_region(&mut regions, "terms", &coords.regions.terms);
        insert_region(&mut regions, "footer", &coords.regions.footer);

        let columns: Vec<TableColumn> = coords
            .table
            .columns
            .into_iter()
            .map(|c| TableColumn {
                name: c.name,
                left: c.left,
                right: c.right,
                alignment: c.alignment,
            })
            .collect();

        Ok(Self {
            page_width: coords.page.width,
            page_height: coords.page.height,
            regions,
            columns,
            table_header_top: coords.table.header_top,
            table_header_bottom: coords.table.header_bottom,
            table_body_bottom: coords.table.body_bottom,
            table_text_inset: coords.table.text_inset,
            minimum_row_line_height: coords.table.minimum_row_line_height,
        })
    }

    pub fn default_v1() -> Result<Self, LayoutError> {
        Self::from_fixture_json(include_str!(
            "../../../tests/fixtures/quotation/layout-v1.json"
        ))
    }

    pub fn region(&self, name: &'static str) -> Result<Region, LayoutError> {
        self.regions
            .get(name)
            .copied()
            .ok_or(LayoutError::MissingRegion(name))
    }

    pub fn column_by_name(&self, name: &str) -> Option<&TableColumn> {
        self.columns.iter().find(|c| c.name == name)
    }
}

pub fn pt_to_pdf_x(x_pt: f32) -> Mm {
    Mm(x_pt / 72.0 * 25.4)
}

pub fn pt_to_pdf_y(y_pt: f32) -> Mm {
    Mm((PAGE_HEIGHT_PT - y_pt) / 72.0 * 25.4)
}

pub fn pt(value: f32) -> Pt {
    Pt(value)
}
