use std::fmt::{Debug, Display, Formatter};

use domain::identity::{ParticipantId, WorkflowId};
use domain::{WorkflowTimestamp, identity::IdentityError};

pub use crate::external_operation::OperationJournal;

const MAX_IDENTIFIER_LENGTH: usize = 128;
const MAX_PAGE_TOKEN_LENGTH: usize = 2_048;
const MAX_PRESIGNED_LINK_LENGTH: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortValueError {
    Empty { kind: &'static str },
    TooLong { kind: &'static str, maximum: usize },
    InvalidCharacters { kind: &'static str },
    InvalidSecretReference,
    InvalidPresignedObjectLink,
    Identity(IdentityError),
}

impl Display for PortValueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid persistence port value: {self:?}")
    }
}

impl std::error::Error for PortValueError {}

impl From<IdentityError> for PortValueError {
    fn from(value: IdentityError) -> Self {
        Self::Identity(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageRecordId(String);

impl StorageRecordId {
    pub fn new(value: impl Into<String>) -> Result<Self, PortValueError> {
        let value = value.into();
        validate_identifier("storage record id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageKey(String);

impl StorageKey {
    pub fn new(value: impl Into<String>) -> Result<Self, PortValueError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PortValueError::Empty {
                kind: "storage key",
            });
        }
        let matching_prefix = ["raw/", "artifacts/", "history/"]
            .into_iter()
            .find(|prefix| value.starts_with(prefix));
        if value.bytes().any(|byte| byte.is_ascii_control())
            || !matches!(matching_prefix, Some(prefix) if value.len() > prefix.len())
        {
            return Err(PortValueError::InvalidCharacters {
                kind: "storage key",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectClass {
    RawInput,
    Artifact,
    SanitizedHistory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub workflow_id: WorkflowId,
    pub object_id: StorageRecordId,
    pub class: ObjectClass,
    pub storage_key: StorageKey,
    pub byte_length: u64,
    pub sha256: [u8; 32],
    pub media_type: String,
    pub created_at: WorkflowTimestamp,
}

impl StoredObject {
    pub fn validate(&self) -> Result<(), PortValueError> {
        let required_prefix = match self.class {
            ObjectClass::RawInput => "raw/",
            ObjectClass::Artifact => "artifacts/",
            ObjectClass::SanitizedHistory => "history/",
        };
        let required_workflow_prefix = format!("{required_prefix}{}/", self.workflow_id);
        if !self
            .storage_key
            .as_str()
            .starts_with(&required_workflow_prefix)
        {
            return Err(PortValueError::InvalidCharacters {
                kind: "object class and workflow storage prefix",
            });
        }
        validate_bounded_text("media type", &self.media_type, MAX_IDENTIFIER_LENGTH)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageToken(String);

impl PageToken {
    pub fn new(value: impl Into<String>) -> Result<Self, PortValueError> {
        let value = value.into();
        validate_bounded_text("page token", &value, MAX_PAGE_TOKEN_LENGTH)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_token: Option<PageToken>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretReference(String);

impl SecretReference {
    pub fn new(value: impl Into<String>) -> Result<Self, PortValueError> {
        let value = value.into();
        if !value.starts_with("/novus/")
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || value.len() > 512
        {
            return Err(PortValueError::InvalidSecretReference);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// Construct a secret value. `#[doc(hidden)]` — only the `SecretProvider`
    /// adapter should call this. External callers obtain `SecretValue` from
    /// `resolve_secret`. This is a practical limitation: the `SecretProvider`
    /// trait is implemented in the storage crate, which needs to construct
    /// `SecretValue` from SSM responses.
    #[doc(hidden)]
    pub fn new(value: Vec<u8>) -> Result<Self, PortValueError> {
        if value.is_empty() {
            return Err(PortValueError::Empty {
                kind: "secret value",
            });
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl Debug for SecretValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OAuthStateDigest([u8; 32]);

impl OAuthStateDigest {
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthStateRecord {
    pub digest: OAuthStateDigest,
    pub participant: ParticipantId,
    pub pkce_secret_reference: SecretReference,
    pub expires_at: WorkflowTimestamp,
}

/// A presigned S3 URL is a bearer credential. It is redacted by default and
/// can only be accessed through the explicit `as_str` method.
pub struct PresignedObjectLink(String);

impl PresignedObjectLink {
    pub fn new(value: impl Into<String>) -> Result<Self, PortValueError> {
        let value = value.into();
        if !(value.starts_with("https://") || value.starts_with("http://"))
            || value.len() > MAX_PRESIGNED_LINK_LENGTH
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        {
            return Err(PortValueError::InvalidPresignedObjectLink);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Debug for PresignedObjectLink {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PresignedObjectLink([REDACTED])")
    }
}

impl Drop for PresignedObjectLink {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

#[allow(async_fn_in_trait)]
pub trait ObjectStore {
    type Error: Display;

    /// Store a raw-input or artifact object. Implementations **must** reject
    /// `ObjectClass::SanitizedHistory` — history must be published through
    /// the typed `HistoryStore` capability to enforce producer-owned
    /// redaction.
    async fn put(&self, object: &StoredObject, bytes: &[u8]) -> Result<(), Self::Error>;
    /// Retrieve an object's bytes. Implementations **must** re-verify the
    /// content against `object.byte_length` and `object.sha256` before
    /// returning `Ok`; a mismatch must produce an error. This contract is
    /// relied upon by the publication coordinator's ambiguous-put
    /// disambiguation path.
    async fn get(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error>;
}

/// Typed history storage capability. Separated from `ObjectStore` so that
/// raw `put` cannot be used to bypass producer-owned `SanitizedHistory`
/// redaction. Implementations **must** reject any class other than
/// `ObjectClass::SanitizedHistory`. The `put_history` method accepts a
/// typed `&SanitizedHistory` — callers cannot supply raw bytes.
#[allow(async_fn_in_trait)]
pub trait HistoryStore {
    type Error: Display;

    /// Store a sanitized-history object. Implementations serialize the
    /// `SanitizedHistory` internally, verify the bytes against
    /// `object.byte_length` and `object.sha256`, validate the versioned
    /// schema, and reject any class other than `SanitizedHistory`.
    async fn put_history(
        &self,
        object: &StoredObject,
        history: &crate::sanitized_history::SanitizedHistory,
    ) -> Result<(), Self::Error>;
    /// Retrieve a sanitized-history object's bytes with hash/length
    /// re-verification, as per `ObjectStore::get`.
    async fn get_history(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error>;
}

/// Capability for generating short-lived artifact retrieval links. The expiry
/// is fixed by the adapter's validated deployment configuration, not supplied
/// by an untrusted caller.
#[allow(async_fn_in_trait)]
pub trait ArtifactLinkSigner {
    type Error: Display;

    async fn presign_artifact(
        &self,
        object: &StoredObject,
    ) -> Result<PresignedObjectLink, Self::Error>;
}

/// Resolve a secret through a `SecretProvider`. This is the only way for
/// external code to obtain a `SecretValue` — the constructor is `pub(crate)`.
pub async fn resolve_secret<P: SecretProvider>(
    provider: &P,
    reference: &SecretReference,
) -> Result<SecretValue, SecretResolutionError> {
    provider
        .get_secret(reference)
        .await
        .map_err(|_| SecretResolutionError::Provider)
}

/// Error from resolving a secret through a provider. Provider error details
/// are not exposed — only a stable category label is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretResolutionError {
    Provider,
    Empty,
}

impl Display for SecretResolutionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider => f.write_str("secret provider error"),
            Self::Empty => f.write_str("secret provider returned empty bytes"),
        }
    }
}

impl std::error::Error for SecretResolutionError {}

/// Secret provider capability. The `SecretValue` constructor is
/// `#[doc(hidden)]` to discourage external construction; only adapter
/// implementations should call it. `resolve_secret` is the recommended
/// path for external callers.
#[allow(async_fn_in_trait)]
pub trait SecretProvider {
    type Error: Display;

    /// Retrieve a secret value. Implementations must validate, decrypt,
    /// and return the secret wrapped in a `SecretValue`.
    async fn get_secret(&self, reference: &SecretReference) -> Result<SecretValue, Self::Error>;
}

fn validate_identifier(kind: &'static str, value: &str) -> Result<(), PortValueError> {
    validate_bounded_text(kind, value, MAX_IDENTIFIER_LENGTH)?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PortValueError::InvalidCharacters { kind });
    }
    Ok(())
}

fn validate_bounded_text(
    kind: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), PortValueError> {
    if value.is_empty() {
        return Err(PortValueError::Empty { kind });
    }
    if value.len() > maximum {
        return Err(PortValueError::TooLong { kind, maximum });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(PortValueError::InvalidCharacters { kind });
    }
    Ok(())
}
