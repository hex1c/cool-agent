#![deny(unsafe_code)]

//! Secure attachment normalization.
//!
//! Sniffs content type from magic bytes (never trusting declared MIME types),
//! enforces pixel / page / row / cell / decompressed-size limits, and produces
//! a safe, bounded [`NormalizedDocument`] with no executable content.

use crate::config::NormalizationConfig;
use domain::attachment::{AttachmentError, AttachmentKind};
use std::fmt::{Display, Formatter};

mod csv;
mod image;
mod office;
mod pdf;

pub use image::parse_image_dimensions;
pub use office::scan_office;
pub use pdf::scan_pdf;

/// The result of normalizing a single attachment.
///
/// Contains only metadata and optionally bounded extracted text; raw bytes are
/// never echoed. The caller can feed this to the AI prompt safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDocument {
    pub kind: AttachmentKind,
    pub media_type: String,
    pub pages: Option<u32>,
    pub dimensions: Option<(u32, u32)>,
    pub extracted_text: Option<String>,
    pub byte_length: u64,
}

/// Errors produced by the normalization pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizationError {
    /// The sniffed content type does not match the declared media type.
    MediaTypeMismatch { declared: String, sniffed: String },
    /// The magic bytes do not match any supported type.
    UnknownMediaType,
    /// Image header is malformed or truncated.
    MalformedImage,
    /// PDF header or cross-reference is malformed.
    MalformedPdf,
    /// CSV content is not valid UTF-8.
    MalformedCsv,
    /// The PDF declares encryption (`/Encrypt`).
    EncryptedPdf,
    /// An Office document declares encryption (`EncryptedPackage` / `EncryptionInfo`).
    EncryptedOffice,
    /// A ZIP-based Office document contains macro-bearing entries (`vbaProject.bin`).
    MacroDetected,
    /// The Office ZIP structure is malformed or uses unsupported features (e.g. data descriptors).
    MalformedOffice,
    /// The document is a legacy format (.doc / .xls with OLE2 container) that is not yet
    /// supported because the approved parser library is pending approval.
    UnsupportedLegacyFormat,
    /// The PDF page count exceeds the configured limit.
    ExcessivePages { max: u32, actual: u32 },
    /// Image pixel count exceeds the configured limit.
    ExcessivePixels { max: u64, actual: u64 },
    /// CSV row count exceeds the configured limit.
    ExcessiveRows { max: u32, actual: u32 },
    /// CSV cell count exceeds the configured limit.
    ExcessiveCells { max: u64, actual: u64 },
    /// A ZIP entry's uncompressed size exceeds the configured limit.
    ExcessiveUncompressedSize { max_bytes: u64, actual_bytes: u64 },
    /// A compressed stream's declared length exceeds the decompression bound.
    DecompressionBomb { max_bytes: u64, actual_bytes: u64 },
    /// Extracted text exceeds the configured output size limit.
    OversizedOutput { max: u64, actual: u64 },
    /// The normalization configuration is invalid.
    InvalidConfig { reason: String },
}

impl Display for NormalizationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MediaTypeMismatch { declared, sniffed } => {
                write!(
                    f,
                    "media type mismatch: declared {declared:?}, sniffed {sniffed:?}"
                )
            }
            Self::UnknownMediaType => {
                f.write_str("unknown media type — not JPEG, PNG, PDF, CSV, or OOXML")
            }
            Self::MalformedImage => f.write_str("malformed or truncated image header"),
            Self::MalformedPdf => f.write_str("malformed or truncated PDF"),
            Self::MalformedCsv => f.write_str("CSV is not valid UTF-8"),
            Self::EncryptedPdf => f.write_str("PDF is encrypted"),
            Self::EncryptedOffice => f.write_str("Office document is encrypted"),
            Self::MacroDetected => f.write_str("Office document contains macros"),
            Self::MalformedOffice => f.write_str("malformed or unsupported Office ZIP structure"),
            Self::UnsupportedLegacyFormat => {
                f.write_str("legacy .doc/.xls format is not supported")
            }
            Self::ExcessivePages { max, actual } => {
                write!(f, "PDF has {actual} pages (max {max})")
            }
            Self::ExcessivePixels { max, actual } => {
                write!(f, "image has {actual} pixels (max {max})")
            }
            Self::ExcessiveRows { max, actual } => {
                write!(f, "CSV has {actual} rows (max {max})")
            }
            Self::ExcessiveCells { max, actual } => {
                write!(f, "CSV has {actual} cells (max {max})")
            }
            Self::ExcessiveUncompressedSize {
                max_bytes,
                actual_bytes,
            } => {
                write!(
                    f,
                    "Office ZIP entry uncompressed size {actual_bytes} exceeds limit {max_bytes}"
                )
            }
            Self::DecompressionBomb {
                max_bytes,
                actual_bytes,
            } => {
                write!(
                    f,
                    "decompression bomb: stream declares {actual_bytes} bytes (max {max_bytes})"
                )
            }
            Self::OversizedOutput { max, actual } => {
                write!(f, "extracted text is {actual} bytes (max {max})")
            }
            Self::InvalidConfig { reason } => {
                write!(f, "invalid normalization config: {reason}")
            }
        }
    }
}

impl std::error::Error for NormalizationError {}

impl From<AttachmentError> for NormalizationError {
    fn from(_: AttachmentError) -> Self {
        Self::UnknownMediaType
    }
}

/// Sniffed content type determined from magic bytes or structural properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SniffedType {
    Jpeg,
    Png,
    Pdf,
    Csv,
    Ooxml,
}

impl SniffedType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Pdf => "application/pdf",
            Self::Csv => "text/csv",
            Self::Ooxml => "application/vnd.openxmlformats-officedocument",
        }
    }

    /// Returns true when the declared media type is compatible with this sniffed type.
    fn compatible_media_type(self, declared: &str) -> bool {
        match self {
            Self::Jpeg => declared == "image/jpeg",
            Self::Png => declared == "image/png",
            Self::Pdf => declared == "application/pdf",
            Self::Csv => declared == "text/csv" || declared == "application/csv",
            Self::Ooxml => {
                declared
                    == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    || declared
                        == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            }
        }
    }
}

/// Normalize raw bytes into a [`NormalizedDocument`].
///
/// # Errors
///
/// Returns [`NormalizationError`] if the content is unrecognized, malformed,
/// exceeds limits, or the declared media type does not match the sniffed type.
pub fn normalize(
    config: &NormalizationConfig,
    declared_media_type: &str,
    bytes: &[u8],
) -> Result<NormalizedDocument, NormalizationError> {
    validate_config(config)?;

    let declared = normalize_media_type(declared_media_type);
    let sniffed = sniff_media_type(bytes, &declared)?;

    if !sniffed.compatible_media_type(&declared) {
        return Err(NormalizationError::MediaTypeMismatch {
            declared,
            sniffed: sniffed.as_str().to_owned(),
        });
    }

    let kind = AttachmentKind::from_media_type(&declared)?;

    match sniffed {
        SniffedType::Jpeg | SniffedType::Png => {
            let (width, height) = parse_image_dimensions(bytes)?;
            let pixels = u64::from(width) * u64::from(height);
            if pixels > config.max_image_pixels {
                return Err(NormalizationError::ExcessivePixels {
                    max: config.max_image_pixels,
                    actual: pixels,
                });
            }
            Ok(NormalizedDocument {
                kind,
                media_type: declared,
                pages: None,
                dimensions: Some((width, height)),
                extracted_text: None,
                byte_length: bytes.len() as u64,
            })
        }
        SniffedType::Pdf => {
            let pdf_info = scan_pdf(config, bytes)?;
            Ok(NormalizedDocument {
                kind,
                media_type: declared,
                pages: Some(pdf_info.pages),
                dimensions: None,
                extracted_text: pdf_info.text,
                byte_length: bytes.len() as u64,
            })
        }
        SniffedType::Csv => {
            let text = csv::extract_csv(config, bytes)?;
            Ok(NormalizedDocument {
                kind,
                media_type: declared,
                pages: None,
                dimensions: None,
                extracted_text: Some(text),
                byte_length: bytes.len() as u64,
            })
        }
        SniffedType::Ooxml => {
            office::scan_office(config, bytes, &declared)?;
            Ok(NormalizedDocument {
                kind,
                media_type: declared,
                pages: None,
                dimensions: None,
                extracted_text: None,
                byte_length: bytes.len() as u64,
            })
        }
    }
}

/// Detect content type from magic bytes or structural properties.
/// Never trusts filename or metadata.
///
/// For text-based types (CSV) that lack magic bytes, the `declared` type is
/// used to constrain the sniffing strategy.
fn sniff_media_type(bytes: &[u8], declared: &str) -> Result<SniffedType, NormalizationError> {
    // JPEG: FF D8 FF
    if bytes.get(..3) == Some(&[0xFF, 0xD8, 0xFF]) {
        return Ok(SniffedType::Jpeg);
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if bytes.get(..8) == Some(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Ok(SniffedType::Png);
    }

    // PDF: %PDF-
    if bytes.get(..5) == Some(&[0x25, 0x50, 0x44, 0x46, 0x2D]) {
        return Ok(SniffedType::Pdf);
    }

    // OOXML: ZIP magic PK\x03\x04
    if bytes.get(..4) == Some(&[0x50, 0x4B, 0x03, 0x04]) {
        return Ok(SniffedType::Ooxml);
    }

    // OLE2 magic D0 CF 11 E0 A1 B1 1A E1 (legacy .doc / .xls)
    if bytes.get(..8) == Some(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Err(NormalizationError::UnsupportedLegacyFormat);
    }

    // CSV: no magic bytes — validate as UTF-8 text if declared type suggests CSV
    if declared == "text/csv" || declared == "application/csv" {
        if std::str::from_utf8(bytes).is_ok() {
            return Ok(SniffedType::Csv);
        }
        return Err(NormalizationError::MalformedCsv);
    }

    Err(NormalizationError::UnknownMediaType)
}

/// Normalize a media type (strip parameters, trim, lowercase).
fn normalize_media_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Validate that configuration values are non-zero and consistent.
fn validate_config(config: &NormalizationConfig) -> Result<(), NormalizationError> {
    if config.max_image_pixels == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_image_pixels must be non-zero".to_owned(),
        });
    }
    if config.max_pdf_pages == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_pdf_pages must be non-zero".to_owned(),
        });
    }
    if config.max_decompressed_bytes == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_decompressed_bytes must be non-zero".to_owned(),
        });
    }
    if config.max_normalized_text_bytes == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_normalized_text_bytes must be non-zero".to_owned(),
        });
    }
    if config.max_csv_rows == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_csv_rows must be non-zero".to_owned(),
        });
    }
    if config.max_csv_cells == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_csv_cells must be non-zero".to_owned(),
        });
    }
    if config.max_office_uncompressed_bytes == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_office_uncompressed_bytes must be non-zero".to_owned(),
        });
    }
    if config.max_compression_ratio == 0 {
        return Err(NormalizationError::InvalidConfig {
            reason: "max_compression_ratio must be non-zero".to_owned(),
        });
    }
    Ok(())
}
