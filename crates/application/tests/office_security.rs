//! Integration tests for CSV, Word, and Excel normalization security.
//!
//! All byte fixtures are constructed inline (no binary blobs on disk).
//! Coverage:
//! - CSV text extraction with delimiter detection, BOM handling, row/cell limits
//! - OOXML ZIP security screening (macros, encryption, bombs)
//! - Legacy .doc/.xls OLE2 rejection
//! - Macro-enabled MIME types rejected
//!
//! Run with: `cargo test -p application --test office_security`

use application::config::NormalizationConfig;
use application::normalization::{NormalizationError, normalize};

#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
#[cfg(test)]
mod document_normalization {
    use super::*;

    fn default_config() -> NormalizationConfig {
        NormalizationConfig::default()
    }

    // ══════════════════════════════════════════════════════════════════════
    // CSV tests
    // ══════════════════════════════════════════════════════════════════════

    mod csv {
        use super::*;

        #[test]
        fn valid_comma_csv() {
            let csv = b"name,age,city\nalice,30,nyc\nbob,25,sfo\n";
            let doc = normalize(&default_config(), "text/csv", csv).expect("valid CSV");
            assert_eq!(doc.media_type, "text/csv");
            assert!(doc.extracted_text.is_some());
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.contains("name,age,city"));
            assert!(text.contains("alice,30,nyc"));
            assert_eq!(doc.pages, None);
            assert_eq!(doc.dimensions, None);
        }

        #[test]
        fn valid_application_csv() {
            let csv = b"a,b,c\n";
            let doc = normalize(&default_config(), "application/csv", csv).expect("valid CSV");
            assert_eq!(doc.media_type, "application/csv");
            assert!(doc.extracted_text.is_some());
        }

        #[test]
        fn csv_with_bom() {
            let mut csv = vec![0xEF, 0xBB, 0xBF];
            csv.extend_from_slice(b"col1,col2\nval1,val2\n");
            let doc = normalize(&default_config(), "text/csv", &csv).expect("BOM CSV");
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.starts_with("col1,col2"));
        }

        #[test]
        fn csv_semicolon_dialect() {
            let csv = b"a;b;c\nd;e;f\n";
            let doc = normalize(&default_config(), "text/csv", csv).expect("semicolon CSV");
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.contains("a;b;c"));
        }

        #[test]
        fn csv_invalid_utf8_rejected() {
            let csv = &[0x61, 0x2C, 0x62, 0xFF, 0xFE];
            let err = normalize(&default_config(), "text/csv", csv).unwrap_err();
            assert!(matches!(err, NormalizationError::MalformedCsv));
        }

        #[test]
        fn csv_excessive_rows_rejected() {
            let config = NormalizationConfig {
                max_csv_rows: 2,
                ..default_config()
            };
            let csv = b"a,b\nc,d\ne,f\ng,h\n";
            let err = normalize(&config, "text/csv", csv).unwrap_err();
            assert!(matches!(err, NormalizationError::ExcessiveRows { .. }));
        }

        #[test]
        fn csv_excessive_cells_rejected() {
            let config = NormalizationConfig {
                max_csv_cells: 3,
                ..default_config()
            };
            let csv = b"a,b,c\nd,e,f\n";
            let err = normalize(&config, "text/csv", csv).unwrap_err();
            assert!(matches!(err, NormalizationError::ExcessiveCells { .. }));
        }

        #[test]
        fn csv_text_truncated() {
            let config = NormalizationConfig {
                max_normalized_text_bytes: 5,
                ..default_config()
            };
            let csv = b"abcdefghij\n";
            let doc = normalize(&config, "text/csv", csv).expect("should truncate");
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.len() <= 5);
        }

        #[test]
        fn csv_mime_mismatch_rejected() {
            // CSV bytes declared as image/jpeg — sniffing finds no image magic
            let csv = b"a,b,c\n";
            let err = normalize(&default_config(), "image/jpeg", csv).unwrap_err();
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }

        #[test]
        fn csv_with_tab_dialect() {
            let csv = b"a\tb\tc\nd\te\tf\n";
            let doc = normalize(&default_config(), "text/csv", csv).expect("tab CSV");
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.contains("a\tb\tc"));
        }

        #[test]
        fn csv_empty() {
            let doc = normalize(&default_config(), "text/csv", b"").expect("empty CSV");
            let text = doc.extracted_text.as_ref().unwrap();
            assert!(text.is_empty());
        }
    }

    // ══════════════════════════════════════════════════════════════════════
    // Office (OOXML ZIP) tests
    // ══════════════════════════════════════════════════════════════════════

    mod office {
        use super::*;

        // ── helpers ──────────────────────────────────────────────────────

        fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
            let mut buf = Vec::new();
            let mut central_dir = Vec::new();
            let mut cd_offset: u32 = 0;
            let mut cd_entry_count: u16 = 0;

            for &(name, data) in entries {
                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len() as u16;
                let data_len = data.len() as u32;

                buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
                buf.extend_from_slice(&[20, 0]); // version
                buf.extend_from_slice(&[0, 0]); // flags
                buf.extend_from_slice(&[0, 0]); // compression stored
                buf.extend_from_slice(&[0, 0]); // mod time
                buf.extend_from_slice(&[0, 0]); // mod date
                let crc = crc32(data);
                buf.extend_from_slice(&crc.to_le_bytes());
                buf.extend_from_slice(&data_len.to_le_bytes());
                buf.extend_from_slice(&data_len.to_le_bytes());
                buf.extend_from_slice(&name_len.to_le_bytes());
                buf.extend_from_slice(&0u16.to_le_bytes()); // extra
                buf.extend_from_slice(name_bytes);
                buf.extend_from_slice(data);

                central_dir.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
                central_dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0]);
                central_dir.extend_from_slice(&[0, 0, 0, 0]); // time/date
                central_dir.extend_from_slice(&crc.to_le_bytes());
                central_dir.extend_from_slice(&data_len.to_le_bytes());
                central_dir.extend_from_slice(&data_len.to_le_bytes());
                central_dir.extend_from_slice(&name_len.to_le_bytes());
                central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra
                central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment
                central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk
                central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                central_dir.extend_from_slice(&0u32.to_le_bytes()); // external attrs
                central_dir.extend_from_slice(&cd_offset.to_le_bytes());
                central_dir.extend_from_slice(name_bytes);

                cd_offset += 30 + u32::from(name_len) + data_len;
                cd_entry_count += 1;
            }

            let cd_start = buf.len() as u32;
            buf.extend_from_slice(&central_dir);
            buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
            buf.extend_from_slice(&[0, 0, 0, 0]);
            buf.extend_from_slice(&cd_entry_count.to_le_bytes());
            buf.extend_from_slice(&cd_entry_count.to_le_bytes());
            buf.extend_from_slice(&(central_dir.len() as u32).to_le_bytes());
            buf.extend_from_slice(&cd_start.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // comment len
            buf
        }

        fn crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            let table = crc32_table();
            for &byte in data {
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

        // ── valid OOXML ──────────────────────────────────────────────────

        #[test]
        fn valid_docx_passes() {
            let zip = build_zip(&[("[Content_Types].xml", b"<Types/>")]);
            let doc = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &zip,
            )
            .expect("valid docx");
            assert_eq!(doc.extracted_text, None);
            assert_eq!(doc.pages, None);
            assert!(doc.byte_length > 0);
        }

        #[test]
        fn valid_xlsx_passes() {
            let zip = build_zip(&[("[Content_Types].xml", b"<Types/>")]);
            let doc = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                &zip,
            )
            .expect("valid xlsx");
            assert_eq!(doc.extracted_text, None);
        }

        // ── macro detection ──────────────────────────────────────────────

        #[test]
        fn vba_project_rejected() {
            let zip = build_zip(&[("word/vbaProject.bin", b"macro")]);
            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &zip,
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::MacroDetected));
        }

        // ── encryption detection ─────────────────────────────────────────

        #[test]
        fn encrypted_package_rejected() {
            let zip = build_zip(&[("EncryptedPackage", b"enc")]);
            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &zip,
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::EncryptedOffice));
        }

        #[test]
        fn encryption_info_rejected() {
            let zip = build_zip(&[("EncryptionInfo", b"enc")]);
            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &zip,
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::EncryptedOffice));
        }

        // ── bomb detection ───────────────────────────────────────────────

        #[test]
        fn uncompressed_size_bomb_rejected() {
            let config = NormalizationConfig {
                max_office_uncompressed_bytes: 100,
                ..default_config()
            };
            let zip = build_zip(&[("doc.xml", &[0u8; 200])]);
            let err = normalize(
                &config,
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &zip,
            )
            .unwrap_err();
            assert!(matches!(
                err,
                NormalizationError::ExcessiveUncompressedSize { .. }
            ));
        }

        #[test]
        fn plain_text_declared_as_docx_rejected() {
            // Declared as docx but content is plain text — sniffing fails
            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                b"not a zip",
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }

        #[test]
        fn zip_with_data_descriptor_rejected() {
            let name = b"doc.xml";
            let mut buf = Vec::new();
            buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
            buf.extend_from_slice(&[20, 0]);
            buf.extend_from_slice(&[0x08, 0]); // bit 3 set
            buf.extend_from_slice(&[0, 0]); // stored
            buf.extend_from_slice(&[0, 0, 0, 0]); // time/date
            buf.extend_from_slice(&0u32.to_le_bytes()); // crc
            buf.extend_from_slice(&5u32.to_le_bytes());
            buf.extend_from_slice(&5u32.to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(name);
            buf.extend_from_slice(b"hello");

            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                &buf,
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::MalformedOffice));
        }

        // ── legacy OLE2 (.doc / .xls) rejection ──────────────────────────

        #[test]
        fn legacy_doc_ole2_rejected() {
            let ole2 = build_ole2();
            let err = normalize(&default_config(), "application/msword", &ole2).unwrap_err();
            assert!(matches!(err, NormalizationError::UnsupportedLegacyFormat));
        }

        #[test]
        fn legacy_xls_ole2_rejected() {
            let ole2 = build_ole2();
            let err = normalize(&default_config(), "application/vnd.ms-excel", &ole2).unwrap_err();
            assert!(matches!(err, NormalizationError::UnsupportedLegacyFormat));
        }

        fn build_ole2() -> Vec<u8> {
            let mut buf = Vec::new();
            buf.extend_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
            // Pad to minimum size
            buf.resize(512, 0);
            buf
        }

        // ── macro-enabled MIME via ZIP mismatch ──────────────────────────

        #[test]
        fn docx_mime_no_zip_magic_rejected() {
            // Declared as docx MIME but content is plain text (not ZIP)
            let err = normalize(
                &default_config(),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                b"not a zip",
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::UnknownMediaType));
        }

        // ── xlsm / docm / pptm — rejected by collection allowlist ────────
        // (These MIME types are not in the attachment collection's allowed
        //  MIME list, so they would be rejected before normalization. Test
        //  that our sniffing correctly rejects unknown media types.)

        #[test]
        fn zip_bytes_declared_as_generic_zip_rejected() {
            // Valid ZIP bytes declared as application/zip — sniffed as Ooxml
            // but compatible_media_type rejects application/zip
            let err = normalize(
                &default_config(),
                "application/zip",
                &build_zip(&[("f.txt", b"hello")]),
            )
            .unwrap_err();
            assert!(matches!(err, NormalizationError::MediaTypeMismatch { .. }));
        }
    }
}
