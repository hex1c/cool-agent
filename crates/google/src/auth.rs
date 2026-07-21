use std::fmt::{self, Display, Formatter};

use domain::identity::ParticipantId;
use zeroize::Zeroize;

/// Redacted Google access token.
///
/// The token value is zeroized on drop and never appears in debug or display output.
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleAccessToken(String);

impl GoogleAccessToken {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for GoogleAccessToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for GoogleAccessToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleAccessToken([REDACTED])")
    }
}

impl Display for GoogleAccessToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleAccessToken([REDACTED])")
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum AuthError {
    #[error("owner token source error")]
    Source,
    #[error("owner not connected to google")]
    NotConnected,
}

#[allow(async_fn_in_trait)]
pub trait OwnerTokenSource {
    type Error: Display + fmt::Debug;

    async fn access_token(&self, owner: ParticipantId) -> Result<GoogleAccessToken, Self::Error>;
}
