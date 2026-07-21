/// Classification of a message payload for delivery routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadClassification {
    /// The payload is safe for forum topic delivery — no OAuth credentials,
    /// tokens, authorization codes, or credential-bearing error messages.
    TopicSafe,
    /// The payload contains OAuth-sensitive material and must only be sent
    /// through [`crate::delivery::PrivateDelivery`].
    OAuthBearing,
}

/// Heuristic: does this look like a raw Google OAuth authorization code?
///
/// Google auth codes typically start with `4/` followed by a long
/// alphanumeric string.
fn is_raw_oauth_code(lower: &str) -> bool {
    lower.starts_with("4/") && lower.len() >= 12
}

/// Classify a text payload as topic-safe or OAuth-bearing.
///
/// OAuth-bearing payloads include:
/// - Google OAuth URLs (`accounts.google.com/o/oauth2`, `oauth2`)
/// - Authorization codes (patterns like `4/0A...` or `code=`)
/// - Token references (`access_token`, `refresh_token`, `ya29.`)
/// - OAuth configuration commands (`/connect_google`, `/disconnect_google`,
///   `/oauth_status`)
///
/// This is a heuristic for structural separation, not a cryptographic
/// guarantee.  Its purpose is to catch accidental OAuth leaks into forum
/// topics at the type level.
pub fn classify_payload(text: &str) -> PayloadClassification {
    let lower = text.to_lowercase();

    // OAuth configuration commands.
    if lower.contains("/connect_google")
        || lower.contains("/disconnect_google")
        || lower.contains("/oauth_status")
    {
        return PayloadClassification::OAuthBearing;
    }

    // Google OAuth URLs.
    if lower.contains("accounts.google.com/o/oauth2")
        || lower.contains("oauth2")
            && (lower.contains("client_id=")
                || lower.contains("redirect_uri=")
                || lower.contains("response_type=")
                || lower.contains("scope="))
    {
        return PayloadClassification::OAuthBearing;
    }

    // Authorization code patterns — both embedded in URLs and raw codes.
    if lower.contains("authorization code")
        || (lower.contains("code=") && lower.contains("4/"))
        || is_raw_oauth_code(&lower)
    {
        return PayloadClassification::OAuthBearing;
    }

    // Token material.
    if lower.contains("access_token")
        || lower.contains("refresh_token")
        || lower.contains("ya29.")
        || lower.contains("access token")
        || lower.contains("refresh token")
        || lower.starts_with("1//")
    {
        return PayloadClassification::OAuthBearing;
    }

    // OAuth error messages with credential hints.
    if lower.contains("oauth") && lower.contains("error") {
        return PayloadClassification::OAuthBearing;
    }

    PayloadClassification::TopicSafe
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn topic_content_is_topic_safe() {
        let cases = [
            "Stage 2 complete: quotation draft ready.",
            "Please review the attached PDF.",
            "/status",
            "/done",
            "/correct fix the amount",
            "Your workflow is waiting for confirmation.",
        ];
        for case in &cases {
            assert_eq!(
                classify_payload(case),
                PayloadClassification::TopicSafe,
                "expected TopicSafe for: {case}"
            );
        }
    }

    #[test]
    fn oauth_connect_command_is_oauth_bearing() {
        assert_eq!(
            classify_payload("/connect_google"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn oauth_disconnect_command_is_oauth_bearing() {
        assert_eq!(
            classify_payload("/disconnect_google"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn oauth_status_command_is_oauth_bearing() {
        assert_eq!(
            classify_payload("/oauth_status"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn google_oauth_url_is_oauth_bearing() {
        let url = "https://accounts.google.com/o/oauth2/auth?client_id=...";
        assert_eq!(classify_payload(url), PayloadClassification::OAuthBearing);
    }

    #[test]
    fn access_token_is_oauth_bearing() {
        assert_eq!(
            classify_payload("Your access_token is ya29.abc123def"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn refresh_token_is_oauth_bearing() {
        assert_eq!(
            classify_payload("refresh_token: 1//abc"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn authorization_code_is_oauth_bearing() {
        assert_eq!(
            classify_payload("code=4/0AanRRr..."),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn oauth_error_is_oauth_bearing() {
        assert_eq!(
            classify_payload("OAuth error: invalid_grant"),
            PayloadClassification::OAuthBearing
        );
    }

    #[test]
    fn generic_workflow_message_is_not_falsely_classified() {
        // "oauth" substring without error context is not OAuth-bearing.
        // But our current heuristic flags "oauth" + "error". Let's ensure
        // plain workflow messages about "code" or "token" are safe.
        assert_eq!(
            classify_payload("The discount code is SUMMER2024"),
            PayloadClassification::TopicSafe
        );
        assert_eq!(
            classify_payload("Token amount: 500 SOL"),
            PayloadClassification::TopicSafe
        );
    }
}
