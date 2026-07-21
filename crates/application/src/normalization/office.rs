#![deny(unsafe_code)]

//! Security screening for OOXML documents (.docx / .xlsx) via hand-rolled ZIP
//! local-file-header scanning.
//!
//! Text extraction is deferred to the TypeScript document tool (Task 29).
//! This module only validates structural safety: no macros, no encryption,
//! no decompression bombs, no legacy OLE2 containers.
//!
//! Rejected threats:
//! - `vbaProject.bin` → macro-bearing, rejected as [`NormalizationError::MacroDetected`].
//! - `EncryptedPackage` / `EncryptionInfo` → encrypted, rejected as [`NormalizationError::EncryptedOffice`].
//! - entries with bit 3 of the general-purpose flags set (data descriptor) → fail closed.
//! - entries whose uncompressed size exceeds the configured limit.
//! - entries with compression ratio exceeding the configured limit.
//! - total uncompressed size across all entries exceeding the configured limit.
//! - missing ZIP magic or structurally invalid headers.

use crate::config::NormalizationConfig;
use crate::normalization::NormalizationError;

/// Scan an OOXML byte slice for security threats.
///
/// Returns `Ok(())` when the document passes all security checks.
///
/// # Errors
///
/// Returns [`NormalizationError::MalformedOffice`] for structurally invalid
/// ZIP data, [`NormalizationError::MacroDetected`] when `vbaProject.bin` is
/// present, [`NormalizationError::EncryptedOffice`] when encrypted entries
/// are found, or [`NormalizationError::ExcessiveUncompressedSize`] for
/// decompression bombs.
pub fn scan_office(
    config: &NormalizationConfig,
    bytes: &[u8],
    _declared: &str,
) -> Result<(), NormalizationError> {
    let len = bytes.len();

    // Require ZIP magic at offset 0 for OOXML
    if bytes.get(..4) != Some(&[0x50, 0x4B, 0x03, 0x04]) {
        return Err(NormalizationError::MalformedOffice);
    }

    let max_uncompressed = config.max_office_uncompressed_bytes;
    let max_ratio = config.max_compression_ratio;
    let mut total_uncompressed: u64 = 0;
    let mut pos: usize = 0;

    // Scan for local file header signatures
    while pos + 30 <= len {
        if bytes.get(pos..pos + 4) != Some(&[0x50, 0x4B, 0x03, 0x04]) {
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedOffice)?;
            continue;
        }

        // Validate the header looks reasonable before trusting the sizes.
        // A bogus match on file-content bytes should fail these checks.
        let flags = read_u16(bytes, pos + 6)?;
        let compression = read_u16(bytes, pos + 8)?;
        let compressed_size_raw = read_u32(bytes, pos + 18)?;
        let uncompressed_size_raw = read_u32(bytes, pos + 22)?;
        let filename_len = read_u16(bytes, pos + 26)? as usize;
        let extra_len = read_u16(bytes, pos + 28)? as usize;

        // Reject data-descriptor entries (bit 3) — sizes may be zero in the
        // local header and only appear in a trailing descriptor, so we cannot
        // safely validate size limits.
        if (flags & 0x0008) != 0 {
            return Err(NormalizationError::MalformedOffice);
        }

        // Sanity-check header fields to avoid treating random bytes as a header:
        // - compression method must be ≤ 99 (deflate=8, stored=0 common)
        // - filename length must be reasonable (< 512)
        // - extra field length must be reasonable (< 2048)
        // - compressed/uncompressed sizes under 1 GiB (OOXML entries are tiny)
        if compression > 99
            || filename_len > 512
            || extra_len > 2048
            || compressed_size_raw > 1_000_000_000
            || uncompressed_size_raw > 1_000_000_000
        {
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedOffice)?;
            continue;
        }

        // Read filename
        let filename_start = pos + 30;
        let filename_end = filename_start
            .checked_add(filename_len)
            .ok_or(NormalizationError::MalformedOffice)?;
        if filename_end > len {
            return Err(NormalizationError::MalformedOffice);
        }
        let filename_bytes = bytes
            .get(filename_start..filename_end)
            .ok_or(NormalizationError::MalformedOffice)?;
        let filename = std::str::from_utf8(filename_bytes).unwrap_or("");

        // ── threat checks ────────────────────────────────────────────

        if filename.contains("vbaProject.bin") {
            return Err(NormalizationError::MacroDetected);
        }
        if filename.contains("EncryptedPackage") || filename.contains("EncryptionInfo") {
            return Err(NormalizationError::EncryptedOffice);
        }

        // ── size checks ──────────────────────────────────────────────

        let uncompressed_size = u64::from(uncompressed_size_raw);
        let compressed_size = u64::from(compressed_size_raw);

        if uncompressed_size > max_uncompressed {
            return Err(NormalizationError::ExcessiveUncompressedSize {
                max_bytes: max_uncompressed,
                actual_bytes: uncompressed_size,
            });
        }

        total_uncompressed = total_uncompressed
            .checked_add(uncompressed_size)
            .ok_or(NormalizationError::MalformedOffice)?;
        if total_uncompressed > max_uncompressed {
            return Err(NormalizationError::ExcessiveUncompressedSize {
                max_bytes: max_uncompressed,
                actual_bytes: total_uncompressed,
            });
        }

        // Compression ratio check — for compressed entries, the uncompressed
        // size should not exceed compressed × max_ratio. Skip stored entries.
        if compression != 0 && compressed_size > 0 {
            let ratio = uncompressed_size / compressed_size;
            if ratio > max_ratio {
                return Err(NormalizationError::ExcessiveUncompressedSize {
                    max_bytes: max_uncompressed,
                    actual_bytes: uncompressed_size,
                });
            }
        }

        // Advance past this entry's header + file data
        let data_start = filename_end
            .checked_add(extra_len)
            .ok_or(NormalizationError::MalformedOffice)?;
        let entry_end = data_start
            .checked_add(compressed_size as usize)
            .ok_or(NormalizationError::MalformedOffice)?;

        if entry_end > len {
            return Err(NormalizationError::MalformedOffice);
        }
        pos = entry_end;
    }

    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, NormalizationError> {
    let b0 = u16::from(
        bytes
            .get(offset)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    let b1 = u16::from(
        bytes
            .get(offset + 1)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    Ok(b0 | (b1 << 8))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, NormalizationError> {
    let b0 = u32::from(
        bytes
            .get(offset)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    let b1 = u32::from(
        bytes
            .get(offset + 1)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    let b2 = u32::from(
        bytes
            .get(offset + 2)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    let b3 = u32::from(
        bytes
            .get(offset + 3)
            .copied()
            .ok_or(NormalizationError::MalformedOffice)?,
    );
    Ok(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn default_config() -> NormalizationConfig {
        NormalizationConfig::default()
    }

    // ── helpers ──────────────────────────────────────────────────────────

    /// Build a minimal valid ZIP with a single stored entry.
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut central_dir = Vec::new();
        let mut cd_offset: u32 = 0;
        let mut cd_entry_count: u16 = 0;

        for &(name, data) in entries {
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len() as u16;
            let data_len = data.len() as u32;

            // Local file header
            buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]); // signature
            buf.extend_from_slice(&[20, 0]); // version needed (2.0)
            buf.extend_from_slice(&[0, 0]); // flags
            buf.extend_from_slice(&[0, 0]); // compression (stored)
            buf.extend_from_slice(&[0, 0]); // mod time
            buf.extend_from_slice(&[0, 0]); // mod date
            let crc = crc32(data);
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&data_len.to_le_bytes()); // compressed size
            buf.extend_from_slice(&data_len.to_le_bytes()); // uncompressed size
            buf.extend_from_slice(&name_len.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(data);

            // Central directory entry
            central_dir.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
            central_dir.extend_from_slice(&[20, 0]); // version made by
            central_dir.extend_from_slice(&[20, 0]); // version needed
            central_dir.extend_from_slice(&[0, 0]); // flags
            central_dir.extend_from_slice(&[0, 0]); // compression
            central_dir.extend_from_slice(&[0, 0]); // mod time
            central_dir.extend_from_slice(&[0, 0]); // mod date
            central_dir.extend_from_slice(&crc.to_le_bytes());
            central_dir.extend_from_slice(&data_len.to_le_bytes());
            central_dir.extend_from_slice(&data_len.to_le_bytes());
            central_dir.extend_from_slice(&name_len.to_le_bytes());
            central_dir.extend_from_slice(&0u16.to_le_bytes()); // extra
            central_dir.extend_from_slice(&0u16.to_le_bytes()); // comment
            central_dir.extend_from_slice(&0u16.to_le_bytes()); // disk
            central_dir.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central_dir.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central_dir.extend_from_slice(&cd_offset.to_le_bytes()); // local header offset
            central_dir.extend_from_slice(name_bytes);

            // Update position for next entry
            cd_offset += 30 + u32::from(name_len) + data_len;
            cd_entry_count += 1;
        }

        let cd_start = buf.len() as u32;
        buf.extend_from_slice(&central_dir);

        // End of central directory
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&[0, 0]); // disk number
        buf.extend_from_slice(&[0, 0]); // disk with CD
        buf.extend_from_slice(&cd_entry_count.to_le_bytes()); // entries on disk
        buf.extend_from_slice(&cd_entry_count.to_le_bytes()); // total entries
        let cd_size = central_dir.len() as u32;
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_start.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // comment length

        buf
    }

    /// Build a ZIP with a deflated entry (dummy, no actual compression).
    fn build_zip_deflated(name: &str, uncompressed: u32, compressed: u32) -> Vec<u8> {
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len() as u16;

        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&[20, 0]); // version
        buf.extend_from_slice(&[0, 0]); // flags
        buf.extend_from_slice(&[8, 0]); // compression = deflate
        buf.extend_from_slice(&[0, 0]); // mod time
        buf.extend_from_slice(&[0, 0]); // mod date
        buf.extend_from_slice(&0u32.to_le_bytes()); // crc
        buf.extend_from_slice(&compressed.to_le_bytes());
        buf.extend_from_slice(&uncompressed.to_le_bytes());
        buf.extend_from_slice(&name_len.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra
        buf.extend_from_slice(name_bytes);
        // Dummy compressed data
        let data = vec![0u8; compressed as usize];
        buf.extend_from_slice(&data);

        // Central directory
        let cd_start = buf.len() as u32;
        buf.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        buf.extend_from_slice(&[20, 0, 20, 0, 0, 0, 8, 0]);
        buf.extend_from_slice(&[0, 0, 0, 0]); // time/date
        buf.extend_from_slice(&0u32.to_le_bytes()); // crc
        buf.extend_from_slice(&compressed.to_le_bytes());
        buf.extend_from_slice(&uncompressed.to_le_bytes());
        buf.extend_from_slice(&name_len.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra
        buf.extend_from_slice(&0u16.to_le_bytes()); // comment
        buf.extend_from_slice(&0u16.to_le_bytes()); // disk
        buf.extend_from_slice(&0u16.to_le_bytes()); // internal
        buf.extend_from_slice(&0u32.to_le_bytes()); // external
        buf.extend_from_slice(&0u32.to_le_bytes()); // offset
        buf.extend_from_slice(name_bytes);

        // EOCD
        buf.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&46u32.to_le_bytes()); // CD size
        buf.extend_from_slice(&cd_start.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

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

    // ── valid ZIP ────────────────────────────────────────────────────────

    #[test]
    fn valid_docx_zip_passes() {
        let zip = build_zip(&[("[Content_Types].xml", b"<Types/>")]);
        scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .expect("valid docx ZIP should pass");
    }

    #[test]
    fn valid_xlsx_zip_passes() {
        let zip = build_zip(&[("[Content_Types].xml", b"<Types/>")]);
        scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .expect("valid xlsx ZIP should pass");
    }

    #[test]
    fn valid_zip_multiple_entries_passes() {
        let zip = build_zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<document/>"),
            ("_rels/.rels", b"<Relationships/>"),
        ]);
        scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .expect("multi-entry ZIP should pass");
    }

    // ── macro detection ──────────────────────────────────────────────────

    #[test]
    fn vba_project_rejected() {
        let zip = build_zip(&[("word/vbaProject.bin", b"macro")]);
        let err = scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::MacroDetected));
    }

    // ── encryption detection ─────────────────────────────────────────────

    #[test]
    fn encrypted_package_rejected() {
        let zip = build_zip(&[("EncryptedPackage", b"enc")]);
        let err = scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::EncryptedOffice));
    }

    #[test]
    fn encryption_info_rejected() {
        let zip = build_zip(&[("EncryptionInfo", b"enc")]);
        let err = scan_office(
            &default_config(),
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::EncryptedOffice));
    }

    // ── decompression bomb ───────────────────────────────────────────────

    #[test]
    fn huge_uncompressed_entry_rejected() {
        let config = NormalizationConfig {
            max_office_uncompressed_bytes: 1000,
            ..default_config()
        };
        let zip = build_zip(&[("doc.xml", &vec![0u8; 2000])]);
        let err = scan_office(
            &config,
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessiveUncompressedSize { .. }
        ));
    }

    #[test]
    fn compression_ratio_bomb_rejected() {
        let config = NormalizationConfig {
            max_compression_ratio: 10,
            ..default_config()
        };
        // uncompressed=1000, compressed=10 → ratio=100 → > 10
        let zip = build_zip_deflated("bomb.xml", 1000, 10);
        let err = scan_office(
            &config,
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessiveUncompressedSize { .. }
        ));
    }

    #[test]
    fn total_uncompressed_exceeds_limit_rejected() {
        let config = NormalizationConfig {
            max_office_uncompressed_bytes: 100,
            ..default_config()
        };
        let zip = build_zip(&[("a.xml", &[0u8; 60]), ("b.xml", &[0u8; 60])]);
        let err = scan_office(
            &config,
            &zip,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessiveUncompressedSize { .. }
        ));
    }

    // ── malformed ────────────────────────────────────────────────────────

    #[test]
    fn no_zip_magic_rejected() {
        let err = scan_office(
            &default_config(),
            b"not a zip file at all",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::MalformedOffice));
    }

    #[test]
    fn empty_input_rejected() {
        let err = scan_office(
            &default_config(),
            &[],
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::MalformedOffice));
    }

    #[test]
    fn data_descriptor_flag_rejected() {
        // Build a ZIP with bit 3 set
        let name = b"test.xml";
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        buf.extend_from_slice(&[20, 0]); // version
        buf.extend_from_slice(&[0x08, 0]); // flags: bit 3 set
        buf.extend_from_slice(&[0, 0]); // compression stored
        buf.extend_from_slice(&[0, 0]); // mod time
        buf.extend_from_slice(&[0, 0]); // mod date
        buf.extend_from_slice(&0u32.to_le_bytes()); // crc
        buf.extend_from_slice(&5u32.to_le_bytes()); // compressed size
        buf.extend_from_slice(&5u32.to_le_bytes()); // uncompressed
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // extra
        buf.extend_from_slice(name);
        buf.extend_from_slice(b"hello");

        let err = scan_office(
            &default_config(),
            &buf,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )
        .unwrap_err();
        assert!(matches!(err, NormalizationError::MalformedOffice));
    }
}
