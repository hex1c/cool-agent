use std::fmt::{Debug, Display, Formatter};

/// A Google OAuth access token whose value is redacted in all output paths.
/// The exposed value must never enter topics or logs.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for AccessToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}

impl Display for AccessToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken([REDACTED])")
    }
}

impl Drop for AccessToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A Google OAuth refresh token whose value is redacted in all output paths.
/// The exposed value must never enter topics or logs.
#[derive(Clone, PartialEq, Eq)]
pub struct RefreshToken(String);

impl RefreshToken {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for RefreshToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken([REDACTED])")
    }
}

impl Display for RefreshToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken([REDACTED])")
    }
}

impl Drop for RefreshToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A Google OAuth authorization code whose value is redacted in all output paths.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationCode(String);

impl AuthorizationCode {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for AuthorizationCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizationCode([REDACTED])")
    }
}

impl Display for AuthorizationCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizationCode([REDACTED])")
    }
}

impl Drop for AuthorizationCode {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A PKCE code verifier whose value is redacted in all output paths.
#[derive(Clone)]
pub struct PkceVerifier(String);

impl PkceVerifier {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for PkceVerifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("PkceVerifier([REDACTED])")
    }
}

impl Display for PkceVerifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("PkceVerifier([REDACTED])")
    }
}

impl Drop for PkceVerifier {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A short-lived OAuth state value whose value is redacted in all output paths.
#[derive(Clone)]
pub struct OAuthStateValue(String);

impl OAuthStateValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for OAuthStateValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthStateValue([REDACTED])")
    }
}

impl Display for OAuthStateValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthStateValue([REDACTED])")
    }
}

impl Drop for OAuthStateValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Extension trait for zeroizing heap-allocated strings.
trait Zeroize {
    fn zeroize(&mut self);
}

impl Zeroize for String {
    fn zeroize(&mut self) {
        let mut bytes = std::mem::take(self).into_bytes();
        bytes.fill(0);
    }
}
