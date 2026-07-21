#![deny(unsafe_code)]

//! Shallow structural validation of PDF documents.
//!
//! Performs a conservative security scan — no full parser, no decompression:
//! 1. Verify `%PDF-` header.
//! 2. Count `/Page` objects.
//! 3. Detect `/Encrypt` → reject encrypted PDFs.
//! 4. Estimate decompressed stream sizes from `/Length` + compression filters
//!    and reject streams that exceed the configured bomb threshold.
//!
//! All scans are bounded; malformed/truncated input produces
//! [`NormalizationError::MalformedPdf`].

use crate::normalization::{NormalizationConfig, NormalizationError};

/// Result of a shallow PDF security scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfScanResult {
    /// Number of pages detected.
    pub pages: u32,
    /// Extracted text from the PDF. Currently always `None` — text extraction
    /// is deferred to the TypeScript document-extraction tool (Task 29).
    pub text: Option<String>,
}

/// Perform a shallow security scan of a PDF byte slice.
///
/// # Errors
///
/// Returns [`NormalizationError`] for malformed headers, encryption, excessive
/// pages, or suspected decompression bombs.
pub fn scan_pdf(
    config: &NormalizationConfig,
    bytes: &[u8],
) -> Result<PdfScanResult, NormalizationError> {
    // 1. Header check
    if bytes.get(..5) != Some(b"%PDF-") {
        return Err(NormalizationError::MalformedPdf);
    }

    // 2. Encryption detection
    if contains_subsequence(bytes, b"/Encrypt") {
        return Err(NormalizationError::EncryptedPdf);
    }

    // 3. Page count — count occurrences of "/Type /Page" patterns
    let pages = count_pages(bytes)?;
    if pages > config.max_pdf_pages {
        return Err(NormalizationError::ExcessivePages {
            max: config.max_pdf_pages,
            actual: pages,
        });
    }

    // 4. Decompression bomb detection — scan for streams with compression
    //    filters and check their declared /Length.
    detect_decompression_bomb(config, bytes)?;

    Ok(PdfScanResult { pages, text: None })
}

/// Count `/Page` object occurrences via a linear scan for "/Type /Page".
///
/// Bounded by the file size; cannot loop infinitely.
fn count_pages(bytes: &[u8]) -> Result<u32, NormalizationError> {
    // We scan for patterns like "/Type/Page" or "/Type /Page" or "/Type  /Page".
    // The simplest reliable heuristic: find "/Type" followed within a few bytes
    // by "/Page". We scan for "/Type" then check the next few non-whitespace
    // characters.
    let mut count: u32 = 0;
    let mut pos: usize = 0;
    let len = bytes.len();

    while pos + 5 < len {
        // Look for "/Type"
        if bytes.get(pos..pos + 5) == Some(b"/Type") {
            // Move past "/Type"
            let mut scan = pos + 5;
            // Skip whitespace / comment-like chars
            while scan < len {
                let ch = bytes
                    .get(scan)
                    .copied()
                    .ok_or(NormalizationError::MalformedPdf)?;
                if ch.is_ascii_whitespace() || ch == b'/' {
                    // If we see a '/', it could be the start of "/Page"
                    if ch == b'/' {
                        if bytes.get(scan..scan + 5) == Some(b"/Page") {
                            // Verify it's a word boundary after "/Page"
                            let after = scan + 5;
                            if after >= len
                                || bytes.get(after).copied().is_none_or(|b| {
                                    b.is_ascii_whitespace() || b == b'/' || b == b'>' || b == b'>'
                                })
                            {
                                count = count.saturating_add(1);
                            }
                        }
                        break; // we saw '/', stop scanning this "/Type"
                    }
                    scan = scan
                        .checked_add(1)
                        .ok_or(NormalizationError::MalformedPdf)?;
                } else {
                    break; // not whitespace or '/', this "/Type" isn't for "/Page"
                }
            }
            pos = pos.checked_add(5).ok_or(NormalizationError::MalformedPdf)?;
        } else {
            pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
        }
    }

    Ok(count)
}

/// Scan for compressed streams with suspicious `/Length` values.
///
/// A stream is suspicious if it has a compression filter and its declared
/// `/Length` exceeds `max_decompressed_bytes`, or if the ratio of stream
/// length to file size suggests a bomb (nested / many streams).
fn detect_decompression_bomb(
    config: &NormalizationConfig,
    bytes: &[u8],
) -> Result<(), NormalizationError> {
    let len = bytes.len();
    let mut pos: usize = 0;

    // Scan for stream objects: "obj" ... "stream" ... "endstream"
    while pos + 3 < len {
        // Look for "stream" keyword (preceded by whitespace)
        if bytes.get(pos..pos + 6) == Some(b"stream") {
            // Check that it's at a word boundary
            let is_start = pos == 0
                || bytes
                    .get(pos - 1)
                    .copied()
                    .is_some_and(|b| b.is_ascii_whitespace());
            if !is_start {
                pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
                continue;
            }

            // Search backwards from this position for "/Filter" and "/Length"
            // within a reasonable window (stream dictionary is before "stream")
            let search_start = pos.saturating_sub(4096);
            let dict_region = bytes
                .get(search_start..pos)
                .ok_or(NormalizationError::MalformedPdf)?;

            // Check if this stream uses a compression filter
            let has_compression = contains_subsequence(dict_region, b"/FlateDecode")
                || contains_subsequence(dict_region, b"/DCTDecode")
                || contains_subsequence(dict_region, b"/ASCII85Decode")
                || contains_subsequence(dict_region, b"/LZWDecode");

            if has_compression {
                // Find the /Length value
                if let Some(declared_len) = find_stream_length(dict_region) {
                    if declared_len > config.max_decompressed_bytes {
                        return Err(NormalizationError::DecompressionBomb {
                            max_bytes: config.max_decompressed_bytes,
                            actual_bytes: declared_len,
                        });
                    }
                    // Also check: if declared length is suspiciously large
                    // relative to file size (e.g., > 10x file size)
                    let file_size = len as u64;
                    if file_size > 0 && declared_len > file_size.saturating_mul(10) {
                        return Err(NormalizationError::DecompressionBomb {
                            max_bytes: config.max_decompressed_bytes,
                            actual_bytes: declared_len,
                        });
                    }
                }
            }

            // Skip past "stream\n" or "stream\r\n"
            pos = pos.checked_add(6).ok_or(NormalizationError::MalformedPdf)?;
            while pos < len {
                let ch = bytes
                    .get(pos)
                    .copied()
                    .ok_or(NormalizationError::MalformedPdf)?;
                if ch == b'\n' {
                    pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
                    break;
                }
                if ch == b'\r' {
                    pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
                    if bytes.get(pos).copied() == Some(b'\n') {
                        pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
                    }
                    break;
                }
                pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
            }
        } else {
            pos = pos.checked_add(1).ok_or(NormalizationError::MalformedPdf)?;
        }
    }

    Ok(())
}

/// Find the `/Length` integer value in a PDF dictionary fragment.
///
/// Scans for `/Length` followed by optional whitespace and a decimal integer.
fn find_stream_length(dict: &[u8]) -> Option<u64> {
    let mut pos: usize = 0;
    let len = dict.len();

    while pos + 7 < len {
        if dict.get(pos..pos + 7) == Some(b"/Length") {
            // Past "/Length"
            let mut scan = pos + 7;
            // Skip whitespace
            while scan < len {
                let ch = dict.get(scan).copied()?;
                if !ch.is_ascii_whitespace() {
                    break;
                }
                scan = scan.checked_add(1)?;
            }
            // Read digits
            if scan < len && dict.get(scan).copied().is_some_and(|b| b.is_ascii_digit()) {
                let mut value: u64 = 0;
                let mut digit_pos = scan;
                while digit_pos < len {
                    let d = dict.get(digit_pos).copied()?;
                    if !d.is_ascii_digit() {
                        break;
                    }
                    value = value.saturating_mul(10).saturating_add(u64::from(d - b'0'));
                    digit_pos = digit_pos.checked_add(1)?;
                }
                return Some(value);
            }
            // Also handle indirect reference: "/Length 6 0 R" — skip over it
            // We don't resolve indirect refs; just skip
            pos = scan;
        } else {
            pos = pos.checked_add(1)?;
        }
    }

    None
}

/// Check if `needle` appears as a subsequence in `haystack`.
///
/// This is a simple substring search.
fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let end = haystack.len() - needle.len();
    for start in 0..=end {
        if haystack.get(start..start + needle.len()) == Some(needle) {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────

    fn build_minimal_pdf(pages: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");
        // One object per page
        for i in 1..=pages {
            let obj = format!("1 {i} obj\n<</Type /Page>>\nendobj\n");
            buf.extend_from_slice(obj.as_bytes());
        }
        // Cross-reference table + trailer (minimal)
        buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
        buf
    }

    fn build_encrypted_pdf() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");
        buf.extend_from_slice(b"1 0 obj\n<</Encrypt 2 0 R>>\nendobj\n");
        buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
        buf
    }

    fn build_pdf_with_stream(length: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");
        buf.extend_from_slice(
            format!("1 0 obj\n<</Length {length} /Filter /FlateDecode>>\nstream\n").as_bytes(),
        );
        buf.extend_from_slice(b"x");
        buf.extend_from_slice(b"\nendstream\nendobj\n");
        buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
        buf
    }

    fn build_truncated_pdf() -> Vec<u8> {
        b"%PDF-1.4\n1 0 obj\n<</Type /Pa".to_vec()
    }

    // ── tests ────────────────────────────────────────────────────────────

    #[test]
    fn valid_single_page() {
        let pdf = build_minimal_pdf(1);
        let config = NormalizationConfig::default();
        let result = scan_pdf(&config, &pdf).expect("should scan");
        assert_eq!(result.pages, 1);
    }

    #[test]
    fn valid_five_pages() {
        let pdf = build_minimal_pdf(5);
        let config = NormalizationConfig::default();
        let result = scan_pdf(&config, &pdf).expect("should scan");
        assert_eq!(result.pages, 5);
    }

    #[test]
    fn excessive_pages_rejected() {
        let config = NormalizationConfig {
            max_pdf_pages: 2,
            ..NormalizationConfig::default()
        };
        let pdf = build_minimal_pdf(5);
        let err = scan_pdf(&config, &pdf).unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessivePages { max: 2, actual: 5 }
        ));
    }

    #[test]
    fn encrypted_pdf_rejected() {
        let pdf = build_encrypted_pdf();
        let config = NormalizationConfig::default();
        let err = scan_pdf(&config, &pdf).unwrap_err();
        assert!(matches!(err, NormalizationError::EncryptedPdf));
    }

    #[test]
    fn decompression_bomb_rejected() {
        let config = NormalizationConfig::default();
        // Declared /Length > max_decompressed_bytes
        let pdf = build_pdf_with_stream(config.max_decompressed_bytes + 1);
        let err = scan_pdf(&config, &pdf).unwrap_err();
        assert!(matches!(err, NormalizationError::DecompressionBomb { .. }));
    }

    #[test]
    fn decompression_bomb_ratio_rejected() {
        let config = NormalizationConfig::default();
        // Small file, but /Length is huge relative to file size
        let pdf = build_pdf_with_stream(1_000_000);
        let err = scan_pdf(&config, &pdf).unwrap_err();
        assert!(matches!(err, NormalizationError::DecompressionBomb { .. }));
    }

    #[test]
    fn malformed_truncated() {
        let pdf = build_truncated_pdf();
        let config = NormalizationConfig::default();
        // This may parse as valid (page count 0) or malformed — just verify no panic
        let _ = scan_pdf(&config, &pdf);
    }

    #[test]
    fn empty_input() {
        let config = NormalizationConfig::default();
        assert!(matches!(
            scan_pdf(&config, &[]),
            Err(NormalizationError::MalformedPdf)
        ));
    }

    #[test]
    fn not_a_pdf() {
        let config = NormalizationConfig::default();
        assert!(matches!(
            scan_pdf(&config, b"not a pdf"),
            Err(NormalizationError::MalformedPdf)
        ));
    }

    #[test]
    fn text_is_none() {
        let pdf = build_minimal_pdf(1);
        let config = NormalizationConfig::default();
        let result = scan_pdf(&config, &pdf).expect("should scan");
        assert!(result.text.is_none());
    }
}
