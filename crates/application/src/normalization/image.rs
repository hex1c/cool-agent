#![deny(unsafe_code)]

//! Parse image dimensions from JPEG and PNG headers using magic-byte sniffing
//! and hand-rolled header traversal. No external image crates needed.

use crate::normalization::NormalizationError;

/// Parse width and height from a JPEG or PNG byte slice.
///
/// JPEG: locates the first SOF0 (0xFF 0xC0) or SOF2 (0xFF 0xC2) marker and
/// reads height / width as big-endian u16.
///
/// PNG: reads the IHDR chunk immediately following the 8-byte signature.
pub fn parse_image_dimensions(bytes: &[u8]) -> Result<(u32, u32), NormalizationError> {
    // Dispatch by magic
    match bytes.first() {
        Some(&0xFF) => parse_jpeg_dimensions(bytes),
        Some(&0x89) => parse_png_dimensions(bytes),
        _ => Err(NormalizationError::MalformedImage),
    }
}

// ── JPEG ────────────────────────────────────────────────────────────────────

fn parse_jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), NormalizationError> {
    // SOI: FF D8 FF
    if bytes.get(..3) != Some(&[0xFF, 0xD8, 0xFF]) {
        return Err(NormalizationError::MalformedImage);
    }

    let len = bytes.len();
    let mut pos: usize = 2; // skip SOI marker FFD8, start at the FF of the next marker

    // JPEG segment scan: each segment is 0xFF <marker> <length-hi> <length-lo> <data...>
    // Loop bounded by bytes length so it cannot diverge.
    while pos + 1 < len {
        // Look for 0xFF marker byte
        let byte = bytes
            .get(pos)
            .copied()
            .ok_or(NormalizationError::MalformedImage)?;
        if byte != 0xFF {
            // Skip padding / entropy-coded data until we find the next 0xFF
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?;
            continue;
        }

        // Get marker code (the byte after 0xFF)
        pos = pos
            .checked_add(1)
            .ok_or(NormalizationError::MalformedImage)?;
        let marker = bytes
            .get(pos)
            .copied()
            .ok_or(NormalizationError::MalformedImage)?;

        // Skip stuffed byte (0xFF 0x00 in entropy-coded data is not a marker)
        if marker == 0x00 {
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?;
            continue;
        }

        // Skip repeated 0xFF bytes
        if marker == 0xFF {
            continue;
        }

        // Standalone markers with no length field: RSTn (0xD0..0xD7),
        // SOI (0xD8), EOI (0xD9). Advance one byte past the marker code.
        if matches!(marker, 0xD0..=0xD9) || marker == 0x01 {
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?;
            continue;
        }

        // SOF0: baseline DCT, SOF2: progressive DCT
        if marker == 0xC0 || marker == 0xC2 {
            // After the marker byte at `pos`: 2 length + 1 precision +
            // 2 height + 2 width = 7 bytes, so we need pos+7 to be a valid index.
            let needed = pos
                .checked_add(7)
                .ok_or(NormalizationError::MalformedImage)?;
            if needed >= len {
                return Err(NormalizationError::MalformedImage);
            }

            // Layout after marker: [len:2][precision:1][height:2][width:2]
            let height_hi = u16::from(
                bytes
                    .get(pos + 4)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let height_lo = u16::from(
                bytes
                    .get(pos + 5)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let width_hi = u16::from(
                bytes
                    .get(pos + 6)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let width_lo = u16::from(
                bytes
                    .get(pos + 7)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let height = u32::from(height_hi) << 8 | u32::from(height_lo);
            let width = u32::from(width_hi) << 8 | u32::from(width_lo);

            if width == 0 || height == 0 {
                return Err(NormalizationError::MalformedImage);
            }

            return Ok((width, height));
        }

        // All other markers (0xC0..=0xFE excluding the standalone/SOF cases
        // above) carry a 2-byte big-endian length that includes the length
        // bytes themselves. Advance past marker(1) + seg_len bytes.
        if matches!(marker, 0xC0..=0xFE) {
            let seg_start = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?;
            if seg_start
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?
                >= len
            {
                return Err(NormalizationError::MalformedImage);
            }
            let seg_len_hi = u16::from(
                bytes
                    .get(seg_start)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let seg_len_lo = u16::from(
                bytes
                    .get(seg_start + 1)
                    .copied()
                    .ok_or(NormalizationError::MalformedImage)?,
            );
            let seg_len = usize::from(seg_len_hi) << 8 | usize::from(seg_len_lo);
            if seg_len < 2 {
                return Err(NormalizationError::MalformedImage);
            }
            // marker(1) + seg_len bytes (length field + data)
            pos = seg_start
                .checked_add(seg_len)
                .ok_or(NormalizationError::MalformedImage)?;
        } else {
            // Unknown marker — skip past it
            pos = pos
                .checked_add(1)
                .ok_or(NormalizationError::MalformedImage)?;
        }
    }

    Err(NormalizationError::MalformedImage)
}

// ── PNG ─────────────────────────────────────────────────────────────────────

fn parse_png_dimensions(bytes: &[u8]) -> Result<(u32, u32), NormalizationError> {
    // PNG signature: 8 bytes
    if bytes.len() < 33 {
        return Err(NormalizationError::MalformedImage);
    }

    // Skip 8-byte signature, then read IHDR
    // IHDR chunk: 4 bytes length (must be 13), 4 bytes "IHDR",
    // 4 bytes width, 4 bytes height, then 5 bytes other fields, 4 bytes CRC
    let ihdr_type = bytes
        .get(12..16)
        .ok_or(NormalizationError::MalformedImage)?;
    if ihdr_type != b"IHDR" {
        return Err(NormalizationError::MalformedImage);
    }

    let width = read_be_u32(bytes, 16)?;
    let height = read_be_u32(bytes, 20)?;

    if width == 0 || height == 0 {
        return Err(NormalizationError::MalformedImage);
    }

    Ok((width, height))
}

/// Read a big-endian u32 from offset within `bytes`.
fn read_be_u32(bytes: &[u8], offset: usize) -> Result<u32, NormalizationError> {
    let b0 = bytes
        .get(offset)
        .copied()
        .ok_or(NormalizationError::MalformedImage)?;
    let b1 = bytes
        .get(offset + 1)
        .copied()
        .ok_or(NormalizationError::MalformedImage)?;
    let b2 = bytes
        .get(offset + 2)
        .copied()
        .ok_or(NormalizationError::MalformedImage)?;
    let b3 = bytes
        .get(offset + 3)
        .copied()
        .ok_or(NormalizationError::MalformedImage)?;
    Ok(u32::from(b0) << 24 | u32::from(b1) << 16 | u32::from(b2) << 8 | u32::from(b3))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_valid_baseline() {
        // Minimal valid JPEG with SOF0: SOI, APP0 (JFIF), SOF0 (1x1), SOS, EOI
        let jpeg = build_minimal_jpeg(1, 1);
        let (w, h) = parse_image_dimensions(&jpeg).expect("should parse");
        assert_eq!((w, h), (1, 1));
    }

    #[test]
    fn jpeg_valid_640x480() {
        let jpeg = build_minimal_jpeg(640, 480);
        let (w, h) = parse_image_dimensions(&jpeg).expect("should parse");
        assert_eq!((w, h), (640, 480));
    }

    #[test]
    fn jpeg_truncated_header() {
        let jpeg = [0xFF, 0xD8, 0xFF]; // SOI only
        assert!(matches!(
            parse_image_dimensions(&jpeg),
            Err(NormalizationError::MalformedImage)
        ));
    }

    #[test]
    fn jpeg_truncated_sof() {
        // SOI + start of APP0 marker but truncated
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00];
        assert!(matches!(
            parse_image_dimensions(&jpeg),
            Err(NormalizationError::MalformedImage)
        ));
    }

    #[test]
    fn png_valid() {
        let png = build_minimal_png(32, 32);
        let (w, h) = parse_image_dimensions(&png).expect("should parse");
        assert_eq!((w, h), (32, 32));
    }

    #[test]
    fn png_valid_1024x768() {
        let png = build_minimal_png(1024, 768);
        let (w, h) = parse_image_dimensions(&png).expect("should parse");
        assert_eq!((w, h), (1024, 768));
    }

    #[test]
    fn png_truncated() {
        let png = [0x89, 0x50, 0x4E, 0x47]; // partial signature
        assert!(matches!(
            parse_image_dimensions(&png),
            Err(NormalizationError::MalformedImage)
        ));
    }

    #[test]
    fn png_truncated_ihdr() {
        let png = build_minimal_png_header_truncated();
        assert!(matches!(
            parse_image_dimensions(&png),
            Err(NormalizationError::MalformedImage)
        ));
    }

    #[test]
    fn empty_input() {
        assert!(matches!(
            parse_image_dimensions(&[]),
            Err(NormalizationError::MalformedImage)
        ));
    }

    // ── helpers ──────────────────────────────────────────────────────────

    /// Construct a minimal valid JPEG with given dimensions.
    fn build_minimal_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut buf = Vec::new();
        // SOI
        buf.extend_from_slice(&[0xFF, 0xD8]);
        // APP0 (JFIF) — 16 bytes total
        buf.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        buf.extend_from_slice(b"JFIF\0");
        buf.extend_from_slice(&[1, 2]); // version
        buf.push(0); // units
        buf.extend_from_slice(&[0, 1]); // X density
        buf.extend_from_slice(&[0, 1]); // Y density
        buf.push(0); // thumbnail width
        buf.push(0); // thumbnail height
        // SOF0
        let len = 8u16 + 3 * 3; // 8 (base) + 3 components × 3 bytes each
        buf.push(0xFF);
        buf.push(0xC0);
        buf.extend_from_slice(&len.to_be_bytes());
        buf.push(8); // precision
        buf.extend_from_slice(&height.to_be_bytes());
        buf.extend_from_slice(&width.to_be_bytes());
        buf.push(3); // number of components
        // Y component
        buf.extend_from_slice(&[1, 0x11, 0]);
        // Cb component
        buf.extend_from_slice(&[2, 0x11, 1]);
        // Cr component
        buf.extend_from_slice(&[3, 0x11, 1]);
        // SOS
        buf.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x0C, 3]);
        buf.extend_from_slice(&[1, 0]);
        buf.extend_from_slice(&[2, 0x11]);
        buf.extend_from_slice(&[3, 0x11]);
        buf.extend_from_slice(&[0, 0x3F, 0]);
        // Minimal entropy-coded data: stuffing to avoid false markers
        // Use RST markers to make it valid
        buf.push(0xFF);
        buf.push(0xD0); // RST0
        // EOI
        buf.extend_from_slice(&[0xFF, 0xD9]);
        buf
    }

    /// Minimal PNG with given dimensions.
    fn build_minimal_png(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        // Signature
        buf.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR chunk
        let ihdr_data = build_ihdr_chunk(width, height);
        buf.extend_from_slice(&ihdr_data);
        // IEND chunk (empty)
        let iend_crc = crc32(b"IEND", &[]);
        buf.extend_from_slice(&0u32.to_be_bytes()); // length = 0
        buf.extend_from_slice(b"IEND");
        buf.extend_from_slice(&iend_crc.to_be_bytes());
        buf
    }

    /// Build IHDR chunk data (length + type + data + crc).
    fn build_ihdr_chunk(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data.push(8); // bit depth
        data.push(2); // color type (RGB)
        data.push(0); // compression
        data.push(0); // filter
        data.push(0); // interlace

        let crc = crc32(b"IHDR", &data);
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&13u32.to_be_bytes()); // length
        chunk.extend_from_slice(b"IHDR");
        chunk.extend_from_slice(&data);
        chunk.extend_from_slice(&crc.to_be_bytes());
        chunk
    }

    /// Truncated PNG header — signature + partial IHDR.
    fn build_minimal_png_header_truncated() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        buf.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        buf.extend_from_slice(b"IHD"); // truncated type
        buf
    }

    /// Simple CRC-32 (PNG uses standard CRC-32 with polynomial 0xEDB88320).
    fn crc32(ty: &[u8], data: &[u8]) -> u32 {
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
}
