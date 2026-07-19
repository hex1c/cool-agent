use std::fmt::{Display, Formatter};
use std::time::Duration;

use application::ports::{
    ArtifactLinkSigner, HistoryStore, ObjectClass, ObjectStore, PresignedObjectLink, StoredObject,
};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ServerSideEncryption;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

use crate::dynamodb::sha256_to_hex;

// ── constants ──────────────────────────────────────────────────────

pub const BUCKET_NAME_MIN_LENGTH: usize = 3;
pub const BUCKET_NAME_MAX_LENGTH: usize = 63;
pub const PRESIGN_MIN_TTL_SECONDS: u32 = 1;
pub const PRESIGN_MAX_TTL_SECONDS: u32 = 604_800;
const MAX_SANITIZED_HISTORY_DEPTH: usize = 32;
const MAX_SANITIZED_HISTORY_CONTAINER_ITEMS: usize = 10_000;
const MAX_SANITIZED_HISTORY_MESSAGES: usize = 1_000;
const MAX_SANITIZED_HISTORY_CONTENT_BYTES: usize = 65_536;
const SANITIZED_HISTORY_SCHEMA_VERSION: &str = "novus.sanitized-history.v1";

// ── validation errors ──────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3ValidationError {
    BucketNameEmpty,
    BucketNameTooShort {
        length: usize,
        minimum: usize,
    },
    BucketNameTooLong {
        length: usize,
        maximum: usize,
    },
    BucketNameInvalidCharacters,
    HistoryClassRejected,
    NonHistoryClassRejected,
    ByteLimitZero {
        class: &'static str,
    },
    PresignTtlOutOfRange {
        ttl: u32,
    },
    ObjectNotValid,
    BodyLengthMismatch {
        expected: u64,
        actual: u64,
    },
    BodyHashMismatch,
    ClassLimitExceeded {
        class: &'static str,
        limit: u64,
        actual: u64,
    },
    ContentLengthUnavailable,
    NotJsonMediaType,
    InvalidJson,
    InvalidHistorySchema,
    HistoryMessageLimitExceeded,
    HistoryContentTooLarge,
    HistoryNestingTooDeep,
    HistoryContainerTooLarge,
    CredentialKeyPresent,
    CredentialValueMarker,
}

impl Display for S3ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BucketNameEmpty => f.write_str("bucket name is empty"),
            Self::BucketNameTooShort { length, minimum } => {
                write!(f, "bucket name length {length} is below minimum {minimum}")
            }
            Self::BucketNameTooLong { length, maximum } => {
                write!(f, "bucket name length {length} exceeds maximum {maximum}")
            }
            Self::BucketNameInvalidCharacters => {
                f.write_str("bucket name contains invalid characters")
            }
            Self::HistoryClassRejected => {
                f.write_str("SanitizedHistory must be stored through HistoryStore")
            }
            Self::NonHistoryClassRejected => {
                f.write_str("only SanitizedHistory can be stored through HistoryStore")
            }
            Self::ByteLimitZero { class } => {
                write!(f, "{class} byte limit must be positive")
            }
            Self::PresignTtlOutOfRange { ttl } => {
                write!(f, "presign ttl {ttl} is outside 1..=604800")
            }
            Self::ObjectNotValid => f.write_str("stored object failed validation"),
            Self::BodyLengthMismatch { expected, actual } => {
                write!(f, "body length {actual} does not match expected {expected}")
            }
            Self::BodyHashMismatch => f.write_str("body SHA-256 does not match stored digest"),
            Self::ClassLimitExceeded {
                class,
                limit,
                actual,
            } => {
                write!(f, "{class} size {actual} exceeds limit {limit}")
            }
            Self::ContentLengthUnavailable => {
                f.write_str("S3 GetObject did not return content-length")
            }
            Self::NotJsonMediaType => {
                f.write_str("SanitizedHistory media type must be application/json")
            }
            Self::InvalidJson => f.write_str("SanitizedHistory body is not valid JSON"),
            Self::InvalidHistorySchema => {
                f.write_str("SanitizedHistory body does not match the versioned schema")
            }
            Self::HistoryMessageLimitExceeded => {
                f.write_str("SanitizedHistory contains too many messages")
            }
            Self::HistoryContentTooLarge => {
                f.write_str("SanitizedHistory message content exceeds the configured bound")
            }
            Self::HistoryNestingTooDeep => {
                f.write_str("SanitizedHistory nesting exceeds the configured bound")
            }
            Self::HistoryContainerTooLarge => {
                f.write_str("SanitizedHistory container exceeds the configured bound")
            }
            Self::CredentialKeyPresent => {
                f.write_str("SanitizedHistory contains credential-bearing key")
            }
            Self::CredentialValueMarker => {
                f.write_str("SanitizedHistory contains credential marker in string value")
            }
        }
    }
}

impl std::error::Error for S3ValidationError {}

// ── adapter error ──────────────────────────────────────────────────

/// Every S3 adapter error is exposed through this type so that raw
/// AWS-provider responses, request IDs, and retry metadata are never
/// leaked to callers or logs.
#[derive(Debug)]
pub enum S3Error {
    Validation(S3ValidationError),
    Build { operation: &'static str },
    Service { operation: &'static str },
    Presign { operation: &'static str },
}

impl Display for S3Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(err) => err.fmt(f),
            Self::Build { operation } => write!(f, "failed to build S3 {operation}"),
            Self::Service { operation } => write!(f, "S3 {operation} failed"),
            Self::Presign { operation } => write!(f, "S3 presign {operation} failed"),
        }
    }
}

impl std::error::Error for S3Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(err) => Some(err),
            _ => None,
        }
    }
}

impl From<S3ValidationError> for S3Error {
    fn from(err: S3ValidationError) -> Self {
        Self::Validation(err)
    }
}

// ── S3 object store ────────────────────────────────────────────────

/// Owned connection to a single S3 bucket.
///
/// Construct one per Lambda initialisation; the client is expected to be
/// pre-configured with credentials, region, and endpoint.
#[derive(Clone)]
pub struct S3ObjectStore {
    client: aws_sdk_s3::Client,
    bucket: String,
    raw_input_limit_bytes: u64,
    artifact_limit_bytes: u64,
    sanitized_history_limit_bytes: u64,
    presign_ttl_seconds: u32,
}

impl std::fmt::Debug for S3ObjectStore {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3ObjectStore")
            .field("client", &"[REDACTED]")
            .field("bucket", &self.bucket)
            .field("raw_input_limit_bytes", &self.raw_input_limit_bytes)
            .field("artifact_limit_bytes", &self.artifact_limit_bytes)
            .field(
                "sanitized_history_limit_bytes",
                &self.sanitized_history_limit_bytes,
            )
            .field("presign_ttl_seconds", &self.presign_ttl_seconds)
            .finish()
    }
}

impl S3ObjectStore {
    pub fn new(
        client: aws_sdk_s3::Client,
        bucket: impl Into<String>,
        raw_input_limit_bytes: u64,
        artifact_limit_bytes: u64,
        sanitized_history_limit_bytes: u64,
        presign_ttl_seconds: u32,
    ) -> Result<Self, S3ValidationError> {
        let bucket = bucket.into();
        validate_bucket_name(&bucket)?;
        if raw_input_limit_bytes == 0 {
            return Err(S3ValidationError::ByteLimitZero { class: "raw input" });
        }
        if artifact_limit_bytes == 0 {
            return Err(S3ValidationError::ByteLimitZero { class: "artifact" });
        }
        if sanitized_history_limit_bytes == 0 {
            return Err(S3ValidationError::ByteLimitZero {
                class: "sanitized history",
            });
        }
        if !(PRESIGN_MIN_TTL_SECONDS..=PRESIGN_MAX_TTL_SECONDS).contains(&presign_ttl_seconds) {
            return Err(S3ValidationError::PresignTtlOutOfRange {
                ttl: presign_ttl_seconds,
            });
        }
        Ok(Self {
            client,
            bucket,
            raw_input_limit_bytes,
            artifact_limit_bytes,
            sanitized_history_limit_bytes,
            presign_ttl_seconds,
        })
    }

    fn class_limit(&self, class: ObjectClass) -> (u64, &'static str) {
        match class {
            ObjectClass::RawInput => (self.raw_input_limit_bytes, "raw input"),
            ObjectClass::Artifact => (self.artifact_limit_bytes, "artifact"),
            ObjectClass::SanitizedHistory => {
                (self.sanitized_history_limit_bytes, "sanitized history")
            }
        }
    }
}

// ── validation helpers ─────────────────────────────────────────────

fn validate_bucket_name(name: &str) -> Result<(), S3ValidationError> {
    if name.is_empty() {
        return Err(S3ValidationError::BucketNameEmpty);
    }
    if name.len() > BUCKET_NAME_MAX_LENGTH {
        return Err(S3ValidationError::BucketNameTooLong {
            length: name.len(),
            maximum: BUCKET_NAME_MAX_LENGTH,
        });
    }
    if name.len() < BUCKET_NAME_MIN_LENGTH {
        return Err(S3ValidationError::BucketNameTooShort {
            length: name.len(),
            minimum: BUCKET_NAME_MIN_LENGTH,
        });
    }
    if !name.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
    }) || !name
        .starts_with(|character: char| character.is_ascii_lowercase() || character.is_ascii_digit())
        || !name.ends_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        || name.contains("..")
        || name.contains(".-")
        || name.contains("-.")
        || is_ipv4_address(name)
    {
        return Err(S3ValidationError::BucketNameInvalidCharacters);
    }
    Ok(())
}

fn is_ipv4_address(value: &str) -> bool {
    let mut count = 0_usize;
    for component in value.split('.') {
        if component.parse::<u8>().is_err() {
            return false;
        }
        count += 1;
    }
    count == 4
}

fn validate_object(object: &StoredObject) -> Result<(), S3ValidationError> {
    object
        .validate()
        .map_err(|_| S3ValidationError::ObjectNotValid)
}

fn hash_body(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn check_length_and_hash(
    bytes: &[u8],
    expected_len: u64,
    expected_hash: &[u8; 32],
) -> Result<(), S3ValidationError> {
    let actual_len = bytes.len() as u64;
    if actual_len != expected_len {
        return Err(S3ValidationError::BodyLengthMismatch {
            expected: expected_len,
            actual: actual_len,
        });
    }
    let actual_hash = hash_body(bytes);
    if actual_hash != *expected_hash {
        return Err(S3ValidationError::BodyHashMismatch);
    }
    Ok(())
}

const fn object_class_metadata(class: ObjectClass) -> &'static str {
    match class {
        ObjectClass::RawInput => "raw",
        ObjectClass::Artifact => "artifact",
        ObjectClass::SanitizedHistory => "history",
    }
}

// ── sanitized history validation ───────────────────────────────────

/// Normalized JSON keys that always represent credential-bearing material.
const CREDENTIAL_BEARING_KEYS: &[&str] = &[
    "authorization",
    "auth_header",
    "auth_token",
    "oauth_code",
    "oauth_state",
    "pkce",
    "code_verifier",
    "code_challenge",
    "access_key",
    "secret_key",
    "client_secret",
    "secret",
    "password",
    "passwd",
    "access_token",
    "refresh_token",
    "id_token",
    "api_key",
    "apikey",
    "credential",
    "credentials",
    "cookie",
    "set_cookie",
    "smtp_auth",
    "bearer",
    "signature",
    "presigned_url",
    "provider_response",
    "provider_error",
];

/// String-value markers that suggest credential leakage, checked
/// case-insensitively.
const CREDENTIAL_VALUE_MARKERS: &[&str] = &[
    "x-amz-signature=",
    "x-amz-credential=",
    "client_secret=",
    "refresh_token=",
    "access_token=",
    "id_token=",
    "code_verifier=",
    "api_key=",
    "password=",
    "smtp://",
];

/// High-signal regex patterns for unlabeled credentials that a producer
/// might accidentally include in message content without any key name or
/// labeled prefix. Each pattern is anchored to the structural form of the
/// credential, not to surrounding text.
#[allow(clippy::expect_used)] // patterns are compile-time constants
static CREDENTIAL_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    // Patterns are intentionally specific to avoid false positives on
    // ordinary prose. All use case-sensitive matching because these
    // credential formats are structurally case-sensitive.
    vec![
        // AWS access key ID: AKIA/ASIA/ASCA/ANVA followed by 16 upper-case
        // alphanumerics.
        regex::Regex::new(r"(?:AKIA|ASIA|ASCA|ANVA)[0-9A-Z]{16}")
            .expect("valid regex: aws access key"),
        // PEM private key block (any key type).
        regex::Regex::new(
            r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----",
        )
        .expect("valid regex: pem key"),
        // JWT: three base64url segments separated by dots, each starting
        // with ey (the JSON `{` or `{"` prefix in base64url).
        regex::Regex::new(r"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}")
            .expect("valid regex: jwt"),
        // GitHub token: ghp_, gho_, ghu_, ghs_, or ghr_ followed by 36
        // base62 characters.
        regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{36}").expect("valid regex: github token"),
        // Google API key: AIza followed by 35 base64url-safe characters.
        regex::Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("valid regex: google api key"),
        // Slack token: xox[baprs]- followed by at least 10 alphanumeric
        // characters.
        regex::Regex::new(r"xox[baprs]-[0-9A-Za-z-]{10,}").expect("valid regex: slack token"),
        // Stripe key: sk_live_ or rk_live_ followed by alphanumeric
        // characters.
        regex::Regex::new(r"(?:sk|rk)_live_[0-9A-Za-z]{10,}").expect("valid regex: stripe key"),
        // OpenAI API key: sk-proj- or sk- followed by a long base62/base64url
        // string.
        regex::Regex::new(r"sk-proj-[A-Za-z0-9_-]{20,}").expect("valid regex: openai proj key"),
        regex::Regex::new(r"sk-[A-Za-z0-9]{48}").expect("valid regex: openai legacy key"),
        // Anthropic API key: sk-ant-api03- followed by a long string.
        regex::Regex::new(r"sk-ant-api0[0-9]-[A-Za-z0-9_-]{20,}")
            .expect("valid regex: anthropic key"),
        // GitHub fine-grained PAT: github_pat_ followed by base62.
        regex::Regex::new(r"github_pat_[A-Za-z0-9_]{22,}").expect("valid regex: github pat"),
        // npm token: npm_ followed by 36 base62 characters.
        regex::Regex::new(r"npm_[A-Za-z0-9]{36}").expect("valid regex: npm token"),
    ]
});

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SanitizedHistoryDocument {
    schema_version: String,
    messages: Vec<SanitizedHistoryMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizedHistoryMessage {
    #[serde(rename = "role")]
    _role: SanitizedHistoryRole,
    content: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SanitizedHistoryRole {
    System,
    User,
    Assistant,
}

fn validate_sanitized_history(body: &[u8], limit: u64) -> Result<(), S3ValidationError> {
    if body.len() as u64 > limit {
        return Err(S3ValidationError::ClassLimitExceeded {
            class: "sanitized history",
            limit,
            actual: body.len() as u64,
        });
    }

    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| S3ValidationError::InvalidJson)?;

    scan_json_for_credentials(&value, 0)?;
    let document: SanitizedHistoryDocument =
        serde_json::from_value(value).map_err(|_| S3ValidationError::InvalidHistorySchema)?;
    if document.schema_version != SANITIZED_HISTORY_SCHEMA_VERSION {
        return Err(S3ValidationError::InvalidHistorySchema);
    }
    if document.messages.len() > MAX_SANITIZED_HISTORY_MESSAGES {
        return Err(S3ValidationError::HistoryMessageLimitExceeded);
    }
    for message in document.messages {
        if message.content.len() > MAX_SANITIZED_HISTORY_CONTENT_BYTES {
            return Err(S3ValidationError::HistoryContentTooLarge);
        }
    }

    Ok(())
}

fn scan_json_for_credentials(
    value: &serde_json::Value,
    depth: usize,
) -> Result<(), S3ValidationError> {
    if depth > MAX_SANITIZED_HISTORY_DEPTH {
        return Err(S3ValidationError::HistoryNestingTooDeep);
    }

    match value {
        serde_json::Value::Object(map) => {
            if map.len() > MAX_SANITIZED_HISTORY_CONTAINER_ITEMS {
                return Err(S3ValidationError::HistoryContainerTooLarge);
            }
            for (key, child) in map {
                if is_credential_bearing_key(key) {
                    return Err(S3ValidationError::CredentialKeyPresent);
                }
                scan_json_for_credentials(child, depth + 1)?;
            }
        }
        serde_json::Value::Array(items) => {
            if items.len() > MAX_SANITIZED_HISTORY_CONTAINER_ITEMS {
                return Err(S3ValidationError::HistoryContainerTooLarge);
            }
            for item in items {
                scan_json_for_credentials(item, depth + 1)?;
            }
        }
        serde_json::Value::String(string) if contains_credential_value(string) => {
            return Err(S3ValidationError::CredentialValueMarker);
        }
        _ => {}
    }
    Ok(())
}

fn is_credential_bearing_key(key: &str) -> bool {
    let normalized = normalize_json_key(key);
    CREDENTIAL_BEARING_KEYS.contains(&normalized.as_str())
        || normalized.starts_with("x_amz_")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.ends_with("_credential")
        || normalized.ends_with("_token")
        || normalized.contains("presigned")
}

fn normalize_json_key(key: &str) -> String {
    let mut normalized = String::with_capacity(key.len());
    let mut previous_was_lowercase_or_digit = false;
    for character in key.chars() {
        if matches!(character, '-' | ' ' | '.') {
            if !normalized.ends_with('_') {
                normalized.push('_');
            }
            previous_was_lowercase_or_digit = false;
        } else if character.is_ascii_uppercase() {
            if previous_was_lowercase_or_digit && !normalized.ends_with('_') {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_was_lowercase_or_digit = false;
        } else {
            normalized.push(character.to_ascii_lowercase());
            previous_was_lowercase_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
        }
    }
    normalized
}

fn contains_credential_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    CREDENTIAL_VALUE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
        || lower.split("bearer").skip(1).any(|suffix| {
            suffix
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_whitespace())
        })
        || contains_credential_pattern(value)
}

/// Detect high-signal credential *patterns* that do not rely on key names or
/// labeled prefixes. These catch raw tokens that a producer might
/// accidentally include in message content without any surrounding marker.
fn contains_credential_pattern(value: &str) -> bool {
    CREDENTIAL_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(value))
}

// ── ObjectStore impl ───────────────────────────────────────────────

impl ObjectStore for S3ObjectStore {
    type Error = S3Error;

    async fn put(&self, object: &StoredObject, bytes: &[u8]) -> Result<(), Self::Error> {
        validate_object(object)?;

        // Reject SanitizedHistory — it must go through HistoryStore to
        // enforce producer-owned redaction.
        if object.class == ObjectClass::SanitizedHistory {
            return Err(S3ValidationError::HistoryClassRejected.into());
        }

        let (limit, class_name) = self.class_limit(object.class);
        let body_len = bytes.len() as u64;
        if body_len > limit {
            return Err(S3ValidationError::ClassLimitExceeded {
                class: class_name,
                limit,
                actual: body_len,
            }
            .into());
        }

        check_length_and_hash(bytes, object.byte_length, &object.sha256)?;

        // SanitizedHistory additional validation
        if object.class == ObjectClass::SanitizedHistory {
            if object.media_type != "application/json" {
                return Err(S3ValidationError::NotJsonMediaType.into());
            }
            validate_sanitized_history(bytes, limit)?;
        }

        // Canonical objects are write-once. Callers must choose a new object
        // identifier/key for changed content; S3 atomically rejects retries
        // that attempt to overwrite an existing key.
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(object.storage_key.as_str())
            .body(ByteStream::from(bytes.to_vec()))
            .if_none_match("*")
            .server_side_encryption(ServerSideEncryption::Aes256)
            .content_type(&object.media_type)
            .metadata("workflow-id", object.workflow_id.as_str())
            .metadata("object-id", object.object_id.as_str())
            .metadata("object-class", object_class_metadata(object.class))
            .metadata("sha256", sha256_to_hex(&object.sha256))
            .metadata("byte-length", object.byte_length.to_string())
            .send()
            .await
            .map_err(|_| S3Error::Service {
                operation: "put object",
            })?;

        Ok(())
    }

    async fn get(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error> {
        validate_object(object)?;

        let (limit, class_name) = self.class_limit(object.class);

        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(object.storage_key.as_str())
            .send()
            .await
            .map_err(|_| S3Error::Service {
                operation: "get object",
            })?;

        // Check content-length before collecting body
        let content_length = output
            .content_length()
            .ok_or(S3ValidationError::ContentLengthUnavailable)
            .and_then(|value| {
                u64::try_from(value).map_err(|_| S3ValidationError::ContentLengthUnavailable)
            })?;

        if content_length > limit {
            return Err(S3ValidationError::ClassLimitExceeded {
                class: class_name,
                limit,
                actual: content_length,
            }
            .into());
        }

        if content_length != object.byte_length {
            return Err(S3ValidationError::BodyLengthMismatch {
                expected: object.byte_length,
                actual: content_length,
            }
            .into());
        }

        let body_bytes = output
            .body
            .collect()
            .await
            .map_err(|_| S3Error::Service {
                operation: "read object body",
            })?
            .into_bytes();

        check_length_and_hash(&body_bytes, object.byte_length, &object.sha256)?;
        if object.class == ObjectClass::SanitizedHistory {
            if object.media_type != "application/json" {
                return Err(S3ValidationError::NotJsonMediaType.into());
            }
            validate_sanitized_history(&body_bytes, limit)?;
        }

        Ok(body_bytes.to_vec())
    }
}

// ── HistoryStore impl ─────────────────────────────────────────────

impl HistoryStore for S3ObjectStore {
    type Error = S3Error;

    async fn put_history(
        &self,
        object: &StoredObject,
        history: &application::sanitized_history::SanitizedHistory,
    ) -> Result<(), Self::Error> {
        validate_object(object)?;

        // Only SanitizedHistory is accepted through this typed path.
        if object.class != ObjectClass::SanitizedHistory {
            return Err(S3ValidationError::NonHistoryClassRejected.into());
        }

        // Serialize the typed SanitizedHistory internally — callers cannot
        // supply raw bytes.
        let bytes = history
            .serialize()
            .map_err(|_| S3ValidationError::InvalidJson)?;

        let (limit, class_name) = self.class_limit(object.class);
        let body_len = bytes.len() as u64;
        if body_len > limit {
            return Err(S3ValidationError::ClassLimitExceeded {
                class: class_name,
                limit,
                actual: body_len,
            }
            .into());
        }

        check_length_and_hash(&bytes, object.byte_length, &object.sha256)?;

        if object.media_type != "application/json" {
            return Err(S3ValidationError::NotJsonMediaType.into());
        }
        validate_sanitized_history(&bytes, limit)?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(object.storage_key.as_str())
            .body(ByteStream::from(bytes))
            .if_none_match("*")
            .server_side_encryption(ServerSideEncryption::Aes256)
            .content_type(&object.media_type)
            .metadata("workflow-id", object.workflow_id.as_str())
            .metadata("object-id", object.object_id.as_str())
            .metadata("object-class", object_class_metadata(object.class))
            .metadata("sha256", sha256_to_hex(&object.sha256))
            .metadata("byte-length", object.byte_length.to_string())
            .send()
            .await
            .map_err(|_| S3Error::Service {
                operation: "put history object",
            })?;

        Ok(())
    }

    async fn get_history(&self, object: &StoredObject) -> Result<Vec<u8>, Self::Error> {
        validate_object(object)?;

        if object.class != ObjectClass::SanitizedHistory {
            return Err(S3ValidationError::NonHistoryClassRejected.into());
        }

        // Delegate to the same read path as ObjectStore::get, which
        // re-verifies length, SHA-256, media type, and sanitized-history
        // schema before returning.
        ObjectStore::get(self, object).await
    }
}

// ── ArtifactLinkSigner impl ────────────────────────────────────────

impl ArtifactLinkSigner for S3ObjectStore {
    type Error = S3Error;

    async fn presign_artifact(
        &self,
        object: &StoredObject,
    ) -> Result<PresignedObjectLink, Self::Error> {
        if object.class != ObjectClass::Artifact {
            return Err(S3ValidationError::ObjectNotValid.into());
        }

        validate_object(object)?;

        let presigning_config =
            PresigningConfig::expires_in(Duration::from_secs(u64::from(self.presign_ttl_seconds)))
                .map_err(|_| S3Error::Build {
                    operation: "presigning configuration",
                })?;
        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(object.storage_key.as_str())
            .presigned(presigning_config)
            .await
            .map_err(|_| S3Error::Presign {
                operation: "generate presigned URL",
            })?;

        let uri = presigned.uri().to_string();

        PresignedObjectLink::new(uri).map_err(|_| S3Error::Build {
            operation: "presigned object link",
        })
    }
}

// ── tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use application::ports::StorageKey;
    use application::ports::StorageRecordId;
    use domain::WorkflowTimestamp;
    use domain::identity::WorkflowId;

    // ── helpers ────────────────────────────────────────────────────

    fn test_client() -> aws_sdk_s3::Client {
        let config = aws_sdk_s3::Config::builder()
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url("http://127.0.0.1:9")
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "s3-unit-test",
            ))
            .behavior_version_latest()
            .build();
        aws_sdk_s3::Client::from_conf(config)
    }

    fn make_object(
        workflow_id: &WorkflowId,
        class: ObjectClass,
        storage_key: &str,
        media_type: &str,
        body: &[u8],
    ) -> StoredObject {
        let hash = hash_body(body);
        StoredObject {
            workflow_id: workflow_id.clone(),
            object_id: StorageRecordId::new("obj-1").expect("valid id"),
            class,
            storage_key: StorageKey::new(storage_key).expect("valid key"),
            byte_length: body.len() as u64,
            sha256: hash,
            media_type: media_type.to_owned(),
            created_at: WorkflowTimestamp::from_unix_seconds(1),
        }
    }

    fn make_workflow_id() -> WorkflowId {
        WorkflowId::new("workflow-1").expect("valid workflow id")
    }

    // ── constructor / validation ───────────────────────────────────

    #[test]
    fn constructor_rejects_invalid_bucket_names() {
        let client = test_client();
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "", 1, 1, 1, 60),
            Err(S3ValidationError::BucketNameEmpty)
        ));
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "ab", 1, 1, 1, 60),
            Err(S3ValidationError::BucketNameTooShort { .. })
        ));
        let long = "a".repeat(BUCKET_NAME_MAX_LENGTH + 1);
        assert!(matches!(
            S3ObjectStore::new(client.clone(), &long, 1, 1, 1, 60),
            Err(S3ValidationError::BucketNameTooLong { .. })
        ));
        for invalid in [
            "Has_Upper",
            "192.168.0.1",
            "invalid..bucket",
            "invalid.-bucket",
            "invalid-.bucket",
        ] {
            assert!(matches!(
                S3ObjectStore::new(client.clone(), invalid, 1, 1, 1, 60),
                Err(S3ValidationError::BucketNameInvalidCharacters)
            ));
        }
    }

    #[test]
    fn constructor_rejects_zero_byte_limits() {
        let client = test_client();
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "valid-bucket", 0, 1, 1, 60),
            Err(S3ValidationError::ByteLimitZero { class: "raw input" })
        ));
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "valid-bucket", 1, 0, 1, 60),
            Err(S3ValidationError::ByteLimitZero { class: "artifact" })
        ));
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "valid-bucket", 1, 1, 0, 60),
            Err(S3ValidationError::ByteLimitZero {
                class: "sanitized history"
            })
        ));
    }

    #[test]
    fn constructor_rejects_out_of_range_presign_ttl() {
        let client = test_client();
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "valid-bucket", 1, 1, 1, 0),
            Err(S3ValidationError::PresignTtlOutOfRange { ttl: 0 })
        ));
        assert!(matches!(
            S3ObjectStore::new(client.clone(), "valid-bucket", 1, 1, 1, 604_801),
            Err(S3ValidationError::PresignTtlOutOfRange { ttl: 604_801 })
        ));
    }

    #[test]
    fn constructor_accepts_valid_config() {
        let client = test_client();
        let store = S3ObjectStore::new(client, "valid-bucket", 1024, 2048, 4096, 3600)
            .expect("valid config");
        let debug = format!("{store:?}");
        assert!(debug.contains("valid-bucket"));
        assert!(debug.contains("[REDACTED]"));
    }

    // ── limit enforcement ──────────────────────────────────────────

    #[test]
    fn class_limit_enforcement() {
        let client = test_client();
        let store =
            S3ObjectStore::new(client, "valid-bucket", 10, 20, 30, 60).expect("valid config");

        assert_eq!(store.class_limit(ObjectClass::RawInput), (10, "raw input"));
        assert_eq!(store.class_limit(ObjectClass::Artifact), (20, "artifact"));
        assert_eq!(
            store.class_limit(ObjectClass::SanitizedHistory),
            (30, "sanitized history")
        );
    }

    // ── hash / length validation ───────────────────────────────────

    #[tokio::test]
    async fn put_rejects_invalid_bodies_before_network_io() {
        let workflow_id = make_workflow_id();
        let store =
            S3ObjectStore::new(test_client(), "valid-bucket", 4, 4, 256, 60).expect("valid config");

        let oversized = make_object(
            &workflow_id,
            ObjectClass::RawInput,
            "raw/workflow-1/input.bin",
            "application/octet-stream",
            b"12345",
        );
        assert!(matches!(
            store.put(&oversized, b"12345").await,
            Err(S3Error::Validation(
                S3ValidationError::ClassLimitExceeded { .. }
            ))
        ));

        let expected_body = b"test";
        let hash_mismatch = make_object(
            &workflow_id,
            ObjectClass::Artifact,
            "artifacts/workflow-1/output.bin",
            "application/octet-stream",
            expected_body,
        );
        assert!(matches!(
            store.put(&hash_mismatch, b"fail").await,
            Err(S3Error::Validation(S3ValidationError::BodyHashMismatch))
        ));

        let unsafe_history = br#"{"refreshToken":"sensitive"}"#;
        // SanitizedHistory must go through HistoryStore::put_history, not
        // ObjectStore::put.
        let history_for_reject = make_object(
            &workflow_id,
            ObjectClass::SanitizedHistory,
            "history/workflow-1/00000000000000000001.json",
            "application/json",
            unsafe_history,
        );
        assert!(matches!(
            store.put(&history_for_reject, unsafe_history).await,
            Err(S3Error::Validation(S3ValidationError::HistoryClassRejected))
        ));
        // Create a SanitizedHistory whose content contains a credential
        // marker that the sanitizer did not redact (defense-in-depth).
        // Build the StoredObject from the serialized bytes so hash/length
        // match, then verify the storage-layer regex catches it.
        let unsafe_sanitized =
            application::sanitized_history::SanitizedHistory::for_testing(vec![(
                application::sanitized_history::SanitizedRole::User,
                "the refresh_token=sensitive was leaked".to_owned(),
            )])
            .expect("should construct");
        let unsafe_serialized = unsafe_sanitized.serialize().expect("should serialize");
        let unsafe_history_obj = make_object(
            &workflow_id,
            ObjectClass::SanitizedHistory,
            "history/workflow-1/00000000000000000002.json",
            "application/json",
            &unsafe_serialized,
        );
        assert!(matches!(
            store
                .put_history(&unsafe_history_obj, &unsafe_sanitized)
                .await,
            Err(S3Error::Validation(
                S3ValidationError::CredentialValueMarker
            ))
        ));
    }

    #[test]
    fn hash_body_produces_sha256() {
        let digest = hash_body(b"hello");
        assert_eq!(digest.len(), 32);
        // Known SHA-256 of "hello"
        let expected = [
            0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
            0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
            0x93, 0x8b, 0x98, 0x24,
        ];
        assert_eq!(digest, expected);
    }

    #[test]
    fn check_length_and_hash_rejects_mismatches() {
        let hash = hash_body(b"test");
        // wrong length
        assert!(matches!(
            check_length_and_hash(b"test", 3, &hash),
            Err(S3ValidationError::BodyLengthMismatch {
                expected: 3,
                actual: 4
            })
        ));
        // wrong hash
        let mut wrong_hash = hash;
        wrong_hash.fill(0);
        assert!(matches!(
            check_length_and_hash(b"test", 4, &wrong_hash),
            Err(S3ValidationError::BodyHashMismatch)
        ));
        // correct
        check_length_and_hash(b"test", 4, &hash).expect("should match");
    }

    // ── sanitized history credential scanning ──────────────────────

    #[test]
    fn sanitized_history_rejects_non_json() {
        let body = b"not json";
        assert!(matches!(
            validate_sanitized_history(body, 1024),
            Err(S3ValidationError::InvalidJson)
        ));
    }

    #[test]
    fn sanitized_history_rejects_credential_keys() {
        let body = br#"{"Authorization": "some-value"}"#;
        assert!(matches!(
            validate_sanitized_history(body, 1024),
            Err(S3ValidationError::CredentialKeyPresent)
        ));

        let body2 = br#"{"data": {"api_key": "val"}}"#;
        assert!(matches!(
            validate_sanitized_history(body2, 1024),
            Err(S3ValidationError::CredentialKeyPresent)
        ));

        for body in [
            br#"{"data":{"ACCESS_KEY":"val"}}"#.as_slice(),
            br#"{"refreshToken":"val"}"#.as_slice(),
            br#"{"clientSecret":"val"}"#.as_slice(),
            br#"{"smtpPassword":"val"}"#.as_slice(),
        ] {
            assert!(matches!(
                validate_sanitized_history(body, 1024),
                Err(S3ValidationError::CredentialKeyPresent)
            ));
        }
    }

    #[test]
    fn sanitized_history_rejects_credential_value_markers() {
        let body = br#"{"msg": "Authorization: Bearer xyz"}"#;
        assert!(matches!(
            validate_sanitized_history(body, 1024),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        let body2 = br#"{"url": "https://s3.amazonaws.com/bucket/key?X-Amz-Signature=sensitive"}"#;
        assert!(matches!(
            validate_sanitized_history(body2, 1024),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        let body3 = b"{\"msg\":\"Bearer\\r\\nsensitive\"}";
        assert!(matches!(
            validate_sanitized_history(body3, 1024),
            Err(S3ValidationError::CredentialValueMarker)
        ));
    }

    #[test]
    fn sanitized_history_rejects_unlabeled_credential_patterns() {
        // AWS access key ID
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"use AKIAIOSFODNN7EXAMPLE to connect"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // PEM private key
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"assistant","content":"-----BEGIN RSA PRIVATE KEY-----\nMIIE..."}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // JWT (assembled at runtime to avoid triggering secret scanners)
        let jwt = format!(
            "{}.{}.{}",
            "eyJhbGciOiJIUzI1NiJ9",
            "eyJzdWIiOiIxMjM0In0",
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
        );
        let body = format!(
            r#"{{"schemaVersion":"novus.sanitized-history.v1","messages":[{{"role":"user","content":"{jwt}"}}]}}"#
        );
        assert!(matches!(
            validate_sanitized_history(body.as_bytes(), 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // GitHub token
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"ghp_1234567890abcdefghijklmnopqrstuvwxyzABCD"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // Google API key
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"AIzaSyA1234567890abcdefghijklmnopqrstuvwxyzABCD"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // Slack token
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"assistant","content":"xoxb-1234567890-abcdefghij"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // Stripe live key
        let stripe = concat!("sk_live_", "1234567890abcdefghijklmnopqrstuvwxyz");
        let body = format!(
            r#"{{"schemaVersion":"novus.sanitized-history.v1","messages":[{{"role":"user","content":"{}"}}]}}"#,
            stripe,
        )
        .into_bytes();
        let body: &[u8] = &body;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // OpenAI project key
        let openai = concat!("sk-proj-", "1234567890abcdefghijklmnopQRSTuvwxyz_-");
        let body = format!(
            r#"{{"schemaVersion":"novus.sanitized-history.v1","messages":[{{"role":"user","content":"{}"}}]}}"#,
            openai,
        )
        .into_bytes();
        let body: &[u8] = &body;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // Anthropic API key
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"sk-ant-api03-1234567890abcdefghijklmnopQRSTuv"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // GitHub fine-grained PAT
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"github_pat_1234567890abcdefABCDEFGH"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));

        // npm token
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"npm_1234567890abcdefghijklmnopqrstuvwxyzABCD"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::CredentialValueMarker)
        ));
    }

    #[test]
    fn sanitized_history_rejects_tool_messages() {
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"tool","content":"provider output"}]}"#;
        assert!(matches!(
            validate_sanitized_history(body, 10_000),
            Err(S3ValidationError::InvalidHistorySchema)
        ));
    }

    #[test]
    fn sanitized_history_accepts_clean_json() {
        let body = br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[{"role":"user","content":"the report was presigned; x-amz-request-id was documented"},{"role":"assistant","content":"hello"}]}"#;
        validate_sanitized_history(body, 1024).expect("clean JSON should pass");
    }

    #[test]
    fn sanitized_history_rejects_unknown_fields_and_schema_versions() {
        let unknown =
            br#"{"schemaVersion":"novus.sanitized-history.v1","messages":[],"metadata":{}}"#;
        assert!(matches!(
            validate_sanitized_history(unknown, 1024),
            Err(S3ValidationError::InvalidHistorySchema)
        ));
        let wrong_version = br#"{"schemaVersion":"novus.sanitized-history.v2","messages":[]}"#;
        assert!(matches!(
            validate_sanitized_history(wrong_version, 1024),
            Err(S3ValidationError::InvalidHistorySchema)
        ));
    }

    #[test]
    fn sanitized_history_rejects_message_and_content_limits() {
        let messages = (0..=MAX_SANITIZED_HISTORY_MESSAGES)
            .map(|_| serde_json::json!({ "role": "user", "content": "safe" }))
            .collect::<Vec<_>>();
        let too_many = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": SANITIZED_HISTORY_SCHEMA_VERSION,
            "messages": messages,
        }))
        .expect("message fixture should serialize");
        assert!(matches!(
            validate_sanitized_history(&too_many, 1_000_000),
            Err(S3ValidationError::HistoryMessageLimitExceeded)
        ));

        let too_large = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": SANITIZED_HISTORY_SCHEMA_VERSION,
            "messages": [{
                "role": "user",
                "content": "x".repeat(MAX_SANITIZED_HISTORY_CONTENT_BYTES + 1),
            }],
        }))
        .expect("content fixture should serialize");
        assert!(matches!(
            validate_sanitized_history(&too_large, 1_000_000),
            Err(S3ValidationError::HistoryContentTooLarge)
        ));
    }

    #[test]
    fn sanitized_history_rejects_excessive_nesting() {
        let mut value = serde_json::json!("safe");
        for _ in 0..=MAX_SANITIZED_HISTORY_DEPTH {
            value = serde_json::json!({ "message": value });
        }
        let body = serde_json::to_vec(&value).expect("nested fixture should serialize");
        assert!(matches!(
            validate_sanitized_history(&body, 100_000),
            Err(S3ValidationError::HistoryNestingTooDeep)
        ));
    }

    #[test]
    fn sanitized_history_rejects_oversize() {
        let body = b"{}";
        assert!(matches!(
            validate_sanitized_history(body, 1),
            Err(S3ValidationError::ClassLimitExceeded { .. })
        ));
    }

    // ── error display / redaction ──────────────────────────────────

    #[test]
    fn s3_error_display_does_not_leak_provider_details() {
        let err = S3Error::Service {
            operation: "put object",
        };
        let display = err.to_string();
        assert_eq!(display, "S3 put object failed");
        // Confirm no raw AWS content
        assert!(!display.contains("RequestId"));
        assert!(!display.contains("SdkError"));
    }

    #[test]
    fn presign_error_display_is_redacted() {
        let err = S3Error::Presign {
            operation: "generate presigned URL",
        };
        let display = err.to_string();
        assert_eq!(display, "S3 presign generate presigned URL failed");
    }

    #[test]
    fn presigned_object_link_is_redacted_by_default() {
        let link = PresignedObjectLink::new("https://s3.example.com/bucket/key?X-Amz-Expires=3600")
            .expect("valid link");
        let debug_str = format!("{link:?}");
        assert!(debug_str.contains("REDACTED"));
        // Explicit access works
        assert!(link.as_str().contains("X-Amz-Expires=3600"));
    }

    // ── presign class gate ─────────────────────────────────────────

    #[tokio::test]
    async fn presign_artifact_only_allows_artifact_class() {
        let wf = make_workflow_id();
        let raw_obj = make_object(
            &wf,
            ObjectClass::RawInput,
            "raw/workflow-1/data.bin",
            "application/octet-stream",
            b"hello",
        );
        let hist_obj = make_object(
            &wf,
            ObjectClass::SanitizedHistory,
            "history/workflow-1/data.json",
            "application/json",
            b"{}",
        );

        let store = S3ObjectStore::new(test_client(), "valid-bucket", 1024, 1024, 1024, 60)
            .expect("valid config");
        assert!(matches!(
            store.presign_artifact(&raw_obj).await,
            Err(S3Error::Validation(S3ValidationError::ObjectNotValid))
        ));
        assert!(matches!(
            store.presign_artifact(&hist_obj).await,
            Err(S3Error::Validation(S3ValidationError::ObjectNotValid))
        ));
    }

    // ── presigned URL expiry assertion ─────────────────────────────

    #[tokio::test]
    async fn presigned_url_includes_x_amz_expires_from_config() {
        let creds = aws_sdk_s3::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "s3-presign-unit-test",
        );
        let config = aws_sdk_s3::Config::builder()
            .credentials_provider(creds)
            .region(aws_sdk_s3::config::Region::new("us-east-1"))
            .endpoint_url("http://127.0.0.1:9")
            .behavior_version_latest()
            .build();
        let client = aws_sdk_s3::Client::from_conf(config);

        let fixed_ttl: u32 = 900;
        let store = S3ObjectStore::new(client, "valid-bucket", 1024, 2048, 4096, fixed_ttl)
            .expect("valid config");

        let wf = make_workflow_id();
        let body = b"artifact data";
        let obj = make_object(
            &wf,
            ObjectClass::Artifact,
            "artifacts/workflow-1/output.bin",
            "application/octet-stream",
            body,
        );

        let link = store.presign_artifact(&obj).await.expect("presign ok");
        let url = link.as_str();
        assert!(
            url.contains(&format!("X-Amz-Expires={fixed_ttl}")),
            "presigned URL must contain the configured expiry"
        );
    }
}
