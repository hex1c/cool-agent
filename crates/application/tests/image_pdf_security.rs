#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

//! Security-focused integration tests for image and PDF normalization.
//!
//! All byte fixtures are constructed inline — no external test assets needed.
//! The module hierarchy mirrors the target filter:
//! `cargo test -p application document_normalization image pdf`

use application::config::NormalizationConfig;
use application::normalization::{self, NormalizationError};

// ── shared helpers ──────────────────────────────────────────────────────────

/// Default normalization config for tests.
fn default_config() -> NormalizationConfig {
    NormalizationConfig {
        max_image_pixels: 10_000_000,
        max_pdf_pages: 10,
        max_decompressed_bytes: 1_000_000,
        max_normalized_text_bytes: 10_000,
        max_csv_rows: 10_000,
        max_csv_cells: 100_000,
        max_office_uncompressed_bytes: 52_428_800,
        max_compression_ratio: 100,
    }
}

/// Build a minimal valid JPEG with given dimensions.
fn build_minimal_jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0xFF, 0xD8]); // SOI
    // APP0 (JFIF)
    buf.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
    buf.extend_from_slice(b"JFIF\0");
    buf.extend_from_slice(&[1, 2, 0, 0, 1, 0, 1, 0, 0]);
    // SOF0
    let len = 8u16 + 3 * 3;
    buf.push(0xFF);
    buf.push(0xC0);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.push(8); // precision
    buf.extend_from_slice(&height.to_be_bytes());
    buf.extend_from_slice(&width.to_be_bytes());
    buf.push(3); // components
    buf.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    // SOS
    buf.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3F, 0,
    ]);
    buf.extend_from_slice(&[0xFF, 0xD0]); // RST0
    buf.extend_from_slice(&[0xFF, 0xD9]); // EOI
    buf
}

/// Build a minimal valid PNG with given dimensions.
fn build_minimal_png(width: u32, height: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    // Signature
    buf.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
    // IHDR chunk
    let mut ihdr_data = Vec::new();
    ihdr_data.extend_from_slice(&width.to_be_bytes());
    ihdr_data.extend_from_slice(&height.to_be_bytes());
    ihdr_data.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, color type, compression, filter, interlace
    let crc = png_crc32(b"IHDR", &ihdr_data);
    buf.extend_from_slice(&13u32.to_be_bytes());
    buf.extend_from_slice(b"IHDR");
    buf.extend_from_slice(&ihdr_data);
    buf.extend_from_slice(&crc.to_be_bytes());
    // IEND chunk
    let iend_crc = png_crc32(b"IEND", &[]);
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(b"IEND");
    buf.extend_from_slice(&iend_crc.to_be_bytes());
    buf
}

/// Build a minimal PDF with the given number of pages.
fn build_minimal_pdf(pages: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    for i in 1..=pages {
        let obj = format!("1 {i} obj\n<</Type /Page>>\nendobj\n");
        buf.extend_from_slice(obj.as_bytes());
    }
    buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
    buf
}

/// Build an encrypted PDF.
fn build_encrypted_pdf() -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    buf.extend_from_slice(b"1 0 obj\n<</Encrypt 2 0 R>>\nendobj\n");
    buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
    buf
}

/// Build a PDF with a single FlateDecode stream declaring `length` bytes.
fn build_pdf_with_stream(length: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n");
    buf.extend_from_slice(
        format!("1 0 obj\n<</Length {length} /Filter /FlateDecode>>\nstream\n").as_bytes(),
    );
    buf.push(b'x');
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    buf.extend_from_slice(b"trailer\n<</Size 2>>\nstartxref\n0\n%%EOF\n");
    buf
}

fn png_crc32(ty: &[u8], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let table = crc32_table();
    for &byte in ty.iter().chain(data.iter()) {
        let idx = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ table[idx];
    }
    crc ^ 0xFFFF_FFFF
}

fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            if c & 1 != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
        }
        *entry = c;
    }
    table
}

// ── document_normalization ──────────────────────────────────────────────────

mod document_normalization {
    use super::*;

    mod image {
        use super::*;

        #[test]
        fn sniff_matches_jpeg() {
            let jpeg = build_minimal_jpeg(64, 64);
            let config = default_config();
            let doc =
                normalization::normalize(&config, "image/jpeg", &jpeg).expect("should normalize");
            assert_eq!(doc.media_type, "image/jpeg");
            assert_eq!(doc.dimensions, Some((64, 64)));
            assert!(doc.pages.is_none());
        }

        #[test]
        fn sniff_matches_png() {
            let png = build_minimal_png(32, 32);
            let config = default_config();
            let doc =
                normalization::normalize(&config, "image/png", &png).expect("should normalize");
            assert_eq!(doc.media_type, "image/png");
            assert_eq!(doc.dimensions, Some((32, 32)));
        }

        #[test]
        fn mismatched_declared_type_rejected() {
            let jpeg = build_minimal_jpeg(64, 64);
            let config = default_config();
            let err = normalization::normalize(&config, "image/png", &jpeg).unwrap_err();
            assert!(matches!(err, NormalizationError::MediaTypeMismatch { .. }));
        }

        #[test]
        fn oversized_pixels_rejected() {
            let config = NormalizationConfig {
                max_image_pixels: 100,
                ..default_config()
            };
            // 64x64 = 4096 pixels > 100
            let jpeg = build_minimal_jpeg(64, 64);
            let err = normalization::normalize(&config, "image/jpeg", &jpeg).unwrap_err();
            assert!(matches!(
                err,
                NormalizationError::ExcessivePixels { max: 100, .. }
            ));
        }

        #[test]
        fn malformed_truncated_jpeg_rejected() {
            let config = default_config();
            let truncated = vec![0xFF, 0xD8, 0xFF]; // SOI only
            let err = normalization::normalize(&config, "image/jpeg", &truncated).unwrap_err();
            assert!(matches!(err, NormalizationError::MalformedImage));
        }

        #[test]
        fn malformed_truncated_png_rejected() {
            let config = default_config();
            let truncated = vec![0x89, 0x50, 0x4E, 0x47]; // partial signature
            let err = normalization::normalize(&config, "image/png", &truncated).unwrap_err();
            // Sniff fails first — 4 bytes is not a valid PNG signature
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }

        #[test]
        fn unknown_bytes_rejected() {
            let config = default_config();
            let garbage = vec![0x00, 0x01, 0x02, 0x03];
            let err = normalization::normalize(&config, "image/jpeg", &garbage).unwrap_err();
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }
    }

    mod pdf {
        use super::*;

        #[test]
        fn valid_single_page() {
            let pdf = build_minimal_pdf(1);
            let config = default_config();
            let doc =
                normalization::normalize(&config, "application/pdf", &pdf).expect("should scan");
            assert_eq!(doc.media_type, "application/pdf");
            assert_eq!(doc.pages, Some(1));
            assert!(doc.dimensions.is_none());
        }

        #[test]
        fn valid_five_pages() {
            let pdf = build_minimal_pdf(5);
            let config = default_config();
            let doc =
                normalization::normalize(&config, "application/pdf", &pdf).expect("should scan");
            assert_eq!(doc.pages, Some(5));
        }

        #[test]
        fn excessive_pages_rejected() {
            let config = NormalizationConfig {
                max_pdf_pages: 2,
                ..default_config()
            };
            let pdf = build_minimal_pdf(5);
            let err = normalization::normalize(&config, "application/pdf", &pdf).unwrap_err();
            assert!(matches!(
                err,
                NormalizationError::ExcessivePages { max: 2, actual: 5 }
            ));
        }

        #[test]
        fn encrypted_pdf_rejected() {
            let pdf = build_encrypted_pdf();
            let config = default_config();
            let err = normalization::normalize(&config, "application/pdf", &pdf).unwrap_err();
            assert!(matches!(err, NormalizationError::EncryptedPdf));
        }

        #[test]
        fn decompression_bomb_rejected() {
            let config = default_config();
            // Stream declares length > max_decompressed_bytes
            let pdf = build_pdf_with_stream(config.max_decompressed_bytes + 1);
            let err = normalization::normalize(&config, "application/pdf", &pdf).unwrap_err();
            assert!(matches!(err, NormalizationError::DecompressionBomb { .. }));
        }

        #[test]
        fn pdf_mismatched_declared_type_rejected() {
            let pdf = build_minimal_pdf(1);
            let config = default_config();
            let err = normalization::normalize(&config, "image/jpeg", &pdf).unwrap_err();
            assert!(matches!(err, NormalizationError::MediaTypeMismatch { .. }));
        }

        #[test]
        fn malformed_empty() {
            let config = default_config();
            let err = normalization::normalize(&config, "application/pdf", &[]).unwrap_err();
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }
    }

    mod output_constraints {
        use super::*;

        #[test]
        fn extracted_text_within_bounds() {
            let pdf = build_minimal_pdf(1);
            let config = default_config();
            let doc = normalization::normalize(&config, "application/pdf", &pdf)
                .expect("should normalize");
            // Text extraction is deferred; output must fit within bounds
            if let Some(text) = &doc.extracted_text {
                assert!(
                    (text.len() as u64) <= config.max_normalized_text_bytes,
                    "extracted text exceeds max_normalized_text_bytes"
                );
            }
            // byte_length must be correct
            assert_eq!(doc.byte_length, pdf.len() as u64);
        }
    }
}
