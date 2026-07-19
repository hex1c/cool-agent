use domain::confirmation::{PendingConfirmation, PreviewDigest};
use serde::{Deserialize, Serialize};

/// Callback data embedded in inline keyboard buttons.
///
/// Binds a callback to a specific workflow revision and preview digest so
/// that replayed or stale callbacks are rejected before any side effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallbackData {
    /// The workflow revision this callback was issued for.
    revision: u64,
    /// Hex-encoded SHA-256 preview digest (64 hex chars).
    preview_digest_hex: String,
}

impl CallbackData {
    /// Create a new callback data binding from a revision and digest.
    pub fn new(revision: u64, digest: &PreviewDigest) -> Self {
        Self {
            revision,
            preview_digest_hex: hex_encode(digest.as_bytes()),
        }
    }

    /// The workflow revision this callback expects.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Raw hex-encoded digest string.
    pub fn preview_digest_hex(&self) -> &str {
        &self.preview_digest_hex
    }

    /// Serialize to a compact JSON string suitable for Telegram callback data.
    pub fn to_callback_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize from a Telegram callback data string.
    pub fn from_callback_string(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }
}

/// Reasons a callback can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackValidationError {
    /// The callback data could not be parsed as valid JSON.
    ParseError(String),
    /// The callback's revision does not match the current pending confirmation.
    StaleRevision { expected: u64, received: u64 },
    /// The preview digest has changed since the callback was issued.
    PreviewDigestMismatch,
}

impl core::fmt::Display for CallbackValidationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ParseError(msg) => write!(f, "callback data parse error: {msg}"),
            Self::StaleRevision { expected, received } => {
                write!(
                    f,
                    "stale callback: expected revision {expected}, got {received}"
                )
            }
            Self::PreviewDigestMismatch => {
                f.write_str("preview digest mismatch — callback was issued for a different preview")
            }
        }
    }
}

impl std::error::Error for CallbackValidationError {}

/// Validate callback data against the current pending confirmation.
///
/// A callback is valid only when its encoded revision matches
/// `pending.workflow_revision()` and the hex digest decodes to the same bytes
/// as `pending.preview_digest()`.  Any mismatch is rejected as stale/replayed.
///
/// # Errors
///
/// Returns [`CallbackValidationError::ParseError`] when the raw data is not
/// valid JSON, [`CallbackValidationError::StaleRevision`] when the revision
/// differs, or [`CallbackValidationError::PreviewDigestMismatch`] when the
/// digest does not match.
pub fn validate_callback_against_pending(
    raw_data: &str,
    pending: &PendingConfirmation,
) -> Result<CallbackData, CallbackValidationError> {
    let data = CallbackData::from_callback_string(raw_data)
        .map_err(|e| CallbackValidationError::ParseError(e.to_string()))?;

    let expected_revision = pending.workflow_revision().get();
    if data.revision != expected_revision {
        return Err(CallbackValidationError::StaleRevision {
            expected: expected_revision,
            received: data.revision,
        });
    }

    let expected_digest = pending.preview_digest();
    let expected_digest_bytes = expected_digest.as_bytes();
    let received_bytes = hex_decode(&data.preview_digest_hex)
        .map_err(|_| CallbackValidationError::PreviewDigestMismatch)?;

    if received_bytes.as_slice() != expected_digest_bytes.as_slice() {
        return Err(CallbackValidationError::PreviewDigestMismatch);
    }

    Ok(data)
}

/// Encode bytes as a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        15 => 'f',
        _ => '?',
    }
}

/// Decode a hex string into bytes.
fn hex_decode(hex: &str) -> Result<Vec<u8>, ()> {
    if !hex.len().is_multiple_of(2) {
        return Err(());
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let hi_val = hex_val(hi).ok_or(())?;
        let lo_val = hex_val(lo).ok_or(())?;
        bytes.push(hi_val << 4 | lo_val);
    }
    Ok(bytes)
}

fn hex_val(c: char) -> Option<u8> {
    match c {
        '0' => Some(0),
        '1' => Some(1),
        '2' => Some(2),
        '3' => Some(3),
        '4' => Some(4),
        '5' => Some(5),
        '6' => Some(6),
        '7' => Some(7),
        '8' => Some(8),
        '9' => Some(9),
        'a' | 'A' => Some(10),
        'b' | 'B' => Some(11),
        'c' | 'C' => Some(12),
        'd' | 'D' => Some(13),
        'e' | 'E' => Some(14),
        'f' | 'F' => Some(15),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use domain::confirmation::PreviewDigest;

    use super::*;

    fn zero_digest() -> serde_json::Value {
        serde_json::Value::Array(std::iter::repeat_n(serde_json::Value::from(0), 32).collect())
    }

    fn make_pending(revision: u64, digest_bytes: [u8; 32]) -> PendingConfirmation {
        // PendingConfirmation has private fields; use serde round-trip to construct.
        let digest_val = serde_json::to_value(digest_bytes).expect("serialize [u8; 32]");
        let json = serde_json::json!({
            "confirmation_id": "conf-1",
            "workflow_id": "wf-1",
            "workflow_revision": revision,
            "owner": 42,
            "topic": {
                "chat_id": -1001234567890_i64,
                "message_thread_id": 10
            },
            "preview_digest": digest_val,
            "mutation_target": zero_digest(),
            "action": "start_sheet_or_doc_write",
            "expires_at": 2000000000_u64
        });
        serde_json::from_value(json).expect("valid pending confirmation")
    }

    #[test]
    fn valid_callback_passes_validation() {
        let digest_bytes = [0xabu8; 32];
        let pending = make_pending(3, digest_bytes);
        let data = CallbackData::new(3, &PreviewDigest::new(digest_bytes));
        let raw = data.to_callback_string().expect("serialize");

        let result = validate_callback_against_pending(&raw, &pending);
        assert!(result.is_ok(), "expected ok, got {result:?}");
    }

    #[test]
    fn stale_revision_is_rejected() {
        let digest_bytes = [0xabu8; 32];
        let pending = make_pending(5, digest_bytes);
        let data = CallbackData::new(3, &PreviewDigest::new(digest_bytes));
        let raw = data.to_callback_string().expect("serialize");

        let result = validate_callback_against_pending(&raw, &pending);
        assert!(matches!(
            result,
            Err(CallbackValidationError::StaleRevision {
                expected: 5,
                received: 3
            })
        ));
    }

    #[test]
    fn mismatched_digest_is_rejected() {
        let digest_a = [0xabu8; 32];
        let digest_b = [0xcd_u8; 32];
        let pending = make_pending(3, digest_a);
        let data = CallbackData::new(3, &PreviewDigest::new(digest_b));
        let raw = data.to_callback_string().expect("serialize");

        let result = validate_callback_against_pending(&raw, &pending);
        assert!(matches!(
            result,
            Err(CallbackValidationError::PreviewDigestMismatch)
        ));
    }

    #[test]
    fn invalid_json_callback_is_rejected() {
        let digest_bytes = [0xabu8; 32];
        let pending = make_pending(3, digest_bytes);
        let result = validate_callback_against_pending("not json", &pending);
        assert!(matches!(
            result,
            Err(CallbackValidationError::ParseError(_))
        ));
    }

    #[test]
    fn hex_roundtrip_is_identity() {
        let bytes: Vec<u8> = (0..=255).collect();
        let encoded = hex_encode(&bytes);
        let decoded = hex_decode(&encoded).expect("decode");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn odd_length_hex_is_rejected() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn invalid_hex_chars_are_rejected() {
        assert!(hex_decode("abgz").is_err());
    }
}
