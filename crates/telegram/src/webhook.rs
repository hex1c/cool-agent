use subtle::ConstantTimeEq;

/// Errors that can occur during webhook secret-token verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookError {
    /// The `X-Telegram-Bot-Api-Secret-Token` header is missing.
    MissingToken,
    /// The provided token does not match the expected secret.
    InvalidToken,
}

impl core::fmt::Display for WebhookError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingToken => f.write_str("missing X-Telegram-Bot-Api-Secret-Token header"),
            Self::InvalidToken => f.write_str("webhook secret token mismatch"),
        }
    }
}

impl std::error::Error for WebhookError {}

/// Verifies the `X-Telegram-Bot-Api-Secret-Token` header against a configured
/// expected token using constant-time comparison.
pub struct WebhookVerifier {
    expected_token: String,
}

impl WebhookVerifier {
    /// Create a new verifier with the expected secret token value.
    pub fn new(expected_token: String) -> Self {
        Self { expected_token }
    }

    /// Verify a raw header value against the expected token.
    ///
    /// Returns `Ok(())` when the header is present and matches in constant
    /// time.  Returns `Err(WebhookError::MissingToken)` when the header is
    /// absent and `Err(WebhookError::InvalidToken)` on mismatch.
    pub fn verify(&self, header_value: Option<&str>) -> Result<(), WebhookError> {
        let provided = header_value.ok_or(WebhookError::MissingToken)?;
        let expected_bytes = self.expected_token.as_bytes();
        let provided_bytes = provided.as_bytes();

        if expected_bytes.ct_eq(provided_bytes).into() {
            Ok(())
        } else {
            Err(WebhookError::InvalidToken)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn matching_token_is_accepted() {
        let verifier = WebhookVerifier::new("s3cret".into());
        assert!(verifier.verify(Some("s3cret")).is_ok());
    }

    #[test]
    fn mismatched_token_is_rejected() {
        let verifier = WebhookVerifier::new("s3cret".into());
        assert_eq!(
            verifier.verify(Some("wrong")),
            Err(WebhookError::InvalidToken)
        );
    }

    #[test]
    fn missing_token_is_rejected() {
        let verifier = WebhookVerifier::new("s3cret".into());
        assert_eq!(verifier.verify(None), Err(WebhookError::MissingToken));
    }

    #[test]
    fn empty_token_does_not_match_non_empty_expected() {
        let verifier = WebhookVerifier::new("s3cret".into());
        assert_eq!(verifier.verify(Some("")), Err(WebhookError::InvalidToken));
    }

    #[test]
    fn prefix_match_does_not_leak() {
        let verifier = WebhookVerifier::new("abcdef".into());
        assert_eq!(
            verifier.verify(Some("abc")),
            Err(WebhookError::InvalidToken)
        );
    }
}
