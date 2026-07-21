#![deny(unsafe_code)]

//! CSV text extraction with delimiter detection, encoding validation, and
//! row/cell limits.
//!
//! Only UTF-8 is accepted. A leading BOM (EF BB BF) is stripped before parsing.
//! The delimiter is detected from the first non-empty line by counting
//! comma, semicolon, and tab occurrences.

use crate::config::NormalizationConfig;
use crate::normalization::NormalizationError;

/// Extract text from a CSV byte slice. Returns a bounded plain-text
/// representation: one line per row, cells joined by the detected delimiter.
pub fn extract_csv(
    config: &NormalizationConfig,
    bytes: &[u8],
) -> Result<String, NormalizationError> {
    // Strip optional UTF-8 BOM
    let text_bytes = if bytes.get(..3) == Some(&[0xEF, 0xBB, 0xBF]) {
        bytes.get(3..).ok_or(NormalizationError::MalformedCsv)?
    } else {
        bytes
    };

    let text = std::str::from_utf8(text_bytes).map_err(|_| NormalizationError::MalformedCsv)?;

    // Detect delimiter from the first non-empty line
    let delimiter = detect_delimiter(text);

    // Count rows and cells, enforcing limits
    let max_rows = config.max_csv_rows as usize;
    let max_cells = config.max_cells()?;
    let max_text = config.max_normalized_text_bytes as usize;

    let mut output = String::with_capacity(4096.min(max_text));
    let mut row_count: u32 = 0;
    let mut cell_count: u64 = 0;

    for line in text.lines() {
        if row_count as usize >= max_rows {
            return Err(NormalizationError::ExcessiveRows {
                max: config.max_csv_rows,
                actual: row_count,
            });
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if row_count > 0 {
            output.push('\n');
        }

        let cells: Vec<&str> = trimmed.split(delimiter).collect();
        cell_count = cell_count
            .checked_add(cells.len() as u64)
            .ok_or(NormalizationError::MalformedCsv)?;

        if cell_count > max_cells {
            return Err(NormalizationError::ExcessiveCells {
                max: max_cells,
                actual: cell_count,
            });
        }

        // Rejoin the row with the delimiter for stable output
        let row_text = cells.join(&String::from(delimiter));

        // Truncate if adding this row would exceed the text limit
        let needed = if row_count > 0 {
            output.len() + 1 + row_text.len()
        } else {
            row_text.len()
        };

        if needed <= max_text {
            output.push_str(&row_text);
        } else {
            // If the row itself is larger than max_text, take a prefix
            let available = max_text.saturating_sub(output.len());
            if row_count > 0 {
                // account for the newline
                let line_available = available.saturating_sub(1);
                if line_available > 0 {
                    output.push('\n');
                    output.push_str(&row_text[..row_text.len().min(line_available)]);
                }
            } else {
                output.push_str(&row_text[..row_text.len().min(available)]);
            }
            break;
        }

        row_count = row_count.saturating_add(1);
    }

    Ok(output)
}

/// Detect the most likely delimiter from the first non-empty line.
///
/// Counts commas, semicolons, and tabs; returns the one with the highest count.
/// If none are present, defaults to comma.
fn detect_delimiter(text: &str) -> char {
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");

    let comma = first_line.chars().filter(|&c| c == ',').count();
    let semicolon = first_line.chars().filter(|&c| c == ';').count();
    let tab = first_line.chars().filter(|&c| c == '\t').count();

    if semicolon > comma && semicolon > tab {
        ';'
    } else if tab > comma && tab > semicolon {
        '\t'
    } else {
        ','
    }
}

impl NormalizationConfig {
    fn max_cells(&self) -> Result<u64, NormalizationError> {
        if self.max_csv_cells == 0 {
            return Err(NormalizationError::InvalidConfig {
                reason: "max_csv_cells must be non-zero".to_owned(),
            });
        }
        Ok(self.max_csv_cells)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn default_config() -> NormalizationConfig {
        NormalizationConfig::default()
    }

    // ── delimiter detection ──────────────────────────────────────────────

    #[test]
    fn detects_comma_delimiter() {
        assert_eq!(detect_delimiter("a,b,c"), ',');
    }

    #[test]
    fn detects_semicolon_delimiter() {
        assert_eq!(detect_delimiter("a;b;c"), ';');
    }

    #[test]
    fn detects_tab_delimiter() {
        assert_eq!(detect_delimiter("a\tb\tc"), '\t');
    }

    #[test]
    fn defaults_to_comma_when_no_delimiters() {
        assert_eq!(detect_delimiter("abc"), ',');
    }

    #[test]
    fn prefers_most_frequent_delimiter() {
        // comma appears 3 times, semicolon 1 time
        assert_eq!(detect_delimiter("a,b,c;d,e"), ',');
    }

    // ── valid CSV ────────────────────────────────────────────────────────

    #[test]
    fn simple_csv() {
        let csv = b"name,age\nalice,30\nbob,25\n";
        let result = extract_csv(&default_config(), csv).expect("should parse");
        assert_eq!(result, "name,age\nalice,30\nbob,25");
    }

    #[test]
    fn semicolon_csv() {
        let csv = b"name;age\nalice;30\nbob;25\n";
        let result = extract_csv(&default_config(), csv).expect("should parse");
        assert_eq!(result, "name;age\nalice;30\nbob;25");
    }

    #[test]
    fn csv_with_bom() {
        let mut csv = vec![0xEF, 0xBB, 0xBF];
        csv.extend_from_slice(b"name,age\nalice,30\n");
        let result = extract_csv(&default_config(), &csv).expect("should parse");
        assert_eq!(result, "name,age\nalice,30");
    }

    #[test]
    fn csv_skips_empty_lines() {
        let csv = b"name,age\n\nalice,30\n\nbob,25\n";
        let result = extract_csv(&default_config(), csv).expect("should parse");
        assert_eq!(result, "name,age\nalice,30\nbob,25");
    }

    // ── row limit ────────────────────────────────────────────────────────

    #[test]
    fn excessive_rows_rejected() {
        let config = NormalizationConfig {
            max_csv_rows: 2,
            ..default_config()
        };
        let csv = b"a,b\nc,d\ne,f\ng,h\n";
        let err = extract_csv(&config, csv).unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessiveRows { max: 2, actual: 2 }
        ));
    }

    #[test]
    fn row_limit_single_row_ok() {
        let config = NormalizationConfig {
            max_csv_rows: 1,
            ..default_config()
        };
        let csv = b"a,b\n";
        let result = extract_csv(&config, csv).expect("should parse");
        assert_eq!(result, "a,b");
    }

    // ── cell limit ───────────────────────────────────────────────────────

    #[test]
    fn excessive_cells_rejected() {
        let config = NormalizationConfig {
            max_csv_cells: 4,
            ..default_config()
        };
        let csv = b"a,b,c\nd,e,f\n";
        let err = extract_csv(&config, csv).unwrap_err();
        assert!(matches!(
            err,
            NormalizationError::ExcessiveCells { max: 4, actual: 6 }
        ));
    }

    #[test]
    fn cell_limit_exact_ok() {
        let config = NormalizationConfig {
            max_csv_cells: 4,
            ..default_config()
        };
        let csv = b"a,b\nc,d\n";
        let result = extract_csv(&config, csv).expect("should parse");
        assert_eq!(result, "a,b\nc,d");
    }

    // ── invalid UTF-8 ────────────────────────────────────────────────────

    #[test]
    fn invalid_utf8_rejected() {
        let csv = &[0x61, 0x2C, 0x62, 0xFF, 0xFE];
        let err = extract_csv(&default_config(), csv).unwrap_err();
        assert!(matches!(err, NormalizationError::MalformedCsv));
    }

    #[test]
    fn invalid_utf8_with_bom_rejected() {
        let csv = &[0xEF, 0xBB, 0xBF, 0x61, 0x2C, 0x62, 0xFF, 0xFE];
        let err = extract_csv(&default_config(), csv).unwrap_err();
        assert!(matches!(err, NormalizationError::MalformedCsv));
    }

    // ── text truncation ──────────────────────────────────────────────────

    #[test]
    fn text_truncated_to_limit() {
        let config = NormalizationConfig {
            max_normalized_text_bytes: 10,
            ..default_config()
        };
        let csv = b"abcdefghij,more_data_here\n";
        let result = extract_csv(&config, csv).expect("should parse");
        assert!(result.len() <= 10);
    }

    // ── empty input ──────────────────────────────────────────────────────

    #[test]
    fn empty_csv_produces_empty_text() {
        let result = extract_csv(&default_config(), b"").expect("should parse");
        assert!(result.is_empty());
    }

    #[test]
    fn bom_only_produces_empty_text() {
        let csv = &[0xEF, 0xBB, 0xBF];
        let result = extract_csv(&default_config(), csv).expect("should parse");
        assert!(result.is_empty());
    }
}
