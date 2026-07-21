use std::fmt::{Display, Formatter};

use application::ports::{PortValueError, SecretProvider, SecretReference, SecretValue};
use aws_sdk_ssm::types::ParameterType;

pub const MAX_STANDARD_SECURE_STRING_BYTES: usize = 4_096;
const MAX_ENVIRONMENT_NAME_LENGTH: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretProviderValidationError {
    EnvironmentEmpty,
    EnvironmentTooLong,
    EnvironmentInvalidCharacters,
    ReferenceOutOfScope,
    ParameterMissing,
    ParameterNameMismatch,
    ParameterNotSecureString,
    ParameterValueMissing,
    ParameterValueTooLarge,
}

impl Display for SecretProviderValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvironmentEmpty => formatter.write_str("secret environment is empty"),
            Self::EnvironmentTooLong => formatter.write_str("secret environment is too long"),
            Self::EnvironmentInvalidCharacters => {
                formatter.write_str("secret environment contains invalid characters")
            }
            Self::ReferenceOutOfScope => {
                formatter.write_str("secret reference is outside the configured environment")
            }
            Self::ParameterMissing => formatter.write_str("secret parameter is missing"),
            Self::ParameterNameMismatch => {
                formatter.write_str("secret parameter name does not match its reference")
            }
            Self::ParameterNotSecureString => {
                formatter.write_str("secret parameter is not a SecureString")
            }
            Self::ParameterValueMissing => formatter.write_str("secret parameter has no value"),
            Self::ParameterValueTooLarge => {
                formatter.write_str("secret parameter exceeds the standard-tier size limit")
            }
        }
    }
}

impl std::error::Error for SecretProviderValidationError {}

#[derive(Debug)]
pub enum SecretProviderError {
    Validation(SecretProviderValidationError),
    Service { operation: &'static str },
    SecretValue,
}

impl Display for SecretProviderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::Service { operation } => write!(formatter, "Parameter Store {operation} failed"),
            Self::SecretValue => formatter.write_str("Parameter Store returned an invalid secret"),
        }
    }
}

impl std::error::Error for SecretProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SecretProviderValidationError> for SecretProviderError {
    fn from(value: SecretProviderValidationError) -> Self {
        Self::Validation(value)
    }
}

/// Environment-scoped read-only access to Parameter Store. The client must be
/// preconfigured with the deployment's region, credentials, endpoint, timeout,
/// and retry policy before construction.
#[derive(Clone)]
pub struct SsmSecretProvider {
    client: aws_sdk_ssm::Client,
    allowed_prefix: String,
}

impl std::fmt::Debug for SsmSecretProvider {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SsmSecretProvider")
            .field("client", &"[REDACTED]")
            .field("allowed_prefix", &self.allowed_prefix)
            .finish()
    }
}

impl SsmSecretProvider {
    pub fn new(
        client: aws_sdk_ssm::Client,
        environment: impl AsRef<str>,
    ) -> Result<Self, SecretProviderValidationError> {
        let environment = environment.as_ref();
        validate_environment(environment)?;
        Ok(Self {
            client,
            allowed_prefix: format!("/novus/{environment}/"),
        })
    }

    fn validate_reference(
        &self,
        reference: &SecretReference,
    ) -> Result<(), SecretProviderValidationError> {
        let suffix = reference
            .as_str()
            .strip_prefix(&self.allowed_prefix)
            .ok_or(SecretProviderValidationError::ReferenceOutOfScope)?;
        if suffix.is_empty()
            || suffix.starts_with('/')
            || suffix.ends_with('/')
            || suffix.contains("//")
            || suffix.contains(':')
            || suffix
                .split('/')
                .any(|segment| matches!(segment, "." | ".."))
        {
            return Err(SecretProviderValidationError::ReferenceOutOfScope);
        }
        Ok(())
    }

    fn decode_parameter(
        reference: &SecretReference,
        parameter: aws_sdk_ssm::types::Parameter,
    ) -> Result<SecretValue, SecretProviderError> {
        if parameter.name.as_deref() != Some(reference.as_str()) {
            return Err(SecretProviderValidationError::ParameterNameMismatch.into());
        }
        if parameter.r#type.as_ref() != Some(&ParameterType::SecureString) {
            return Err(SecretProviderValidationError::ParameterNotSecureString.into());
        }
        let value = parameter
            .value
            .ok_or(SecretProviderValidationError::ParameterValueMissing)?;
        if value.len() > MAX_STANDARD_SECURE_STRING_BYTES {
            return Err(SecretProviderValidationError::ParameterValueTooLarge.into());
        }
        SecretValue::new(value.into_bytes())
            .map_err(|_error: PortValueError| SecretProviderError::SecretValue)
    }
}

#[allow(async_fn_in_trait)]
impl SecretProvider for SsmSecretProvider {
    type Error = SecretProviderError;

    async fn get_secret(&self, reference: &SecretReference) -> Result<SecretValue, Self::Error> {
        self.validate_reference(reference)?;
        let output = self
            .client
            .get_parameter()
            .name(reference.as_str())
            .with_decryption(true)
            .send()
            .await
            .map_err(|_| SecretProviderError::Service {
                operation: "get parameter",
            })?;
        let parameter = output
            .parameter
            .ok_or(SecretProviderValidationError::ParameterMissing)?;
        Self::decode_parameter(reference, parameter)
    }
}

fn validate_environment(value: &str) -> Result<(), SecretProviderValidationError> {
    if value.is_empty() {
        return Err(SecretProviderValidationError::EnvironmentEmpty);
    }
    if value.len() > MAX_ENVIRONMENT_NAME_LENGTH {
        return Err(SecretProviderValidationError::EnvironmentTooLong);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !value.starts_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
        || !value.ends_with(|character: char| {
            character.is_ascii_lowercase() || character.is_ascii_digit()
        })
    {
        return Err(SecretProviderValidationError::EnvironmentInvalidCharacters);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn client() -> aws_sdk_ssm::Client {
        let config = aws_sdk_ssm::Config::builder()
            .region(aws_sdk_ssm::config::Region::new("us-east-1"))
            .endpoint_url("http://127.0.0.1:9")
            .credentials_provider(aws_sdk_ssm::config::Credentials::new(
                "test",
                "test",
                None,
                None,
                "ssm-unit-test",
            ))
            .behavior_version_latest()
            .build();
        aws_sdk_ssm::Client::from_conf(config)
    }

    #[test]
    fn constructor_rejects_invalid_environments() {
        for invalid in ["", "Development", "-development", "development-"] {
            assert!(SsmSecretProvider::new(client(), invalid).is_err());
        }
        assert!(SsmSecretProvider::new(client(), "development").is_ok());
    }

    #[test]
    fn references_are_bound_to_exact_environment_prefix() {
        let provider = SsmSecretProvider::new(client(), "development").expect("valid provider");
        let allowed = SecretReference::new("/novus/development/google/client-secret")
            .expect("valid reference");
        assert!(provider.validate_reference(&allowed).is_ok());

        for disallowed in [
            "/novus/staging/google/client-secret",
            "/novus/development-2/google/client-secret",
            "/novus/development//google/client-secret",
            "/novus/development/../staging/google/client-secret",
            "/novus/development/google/client-secret:1",
        ] {
            let reference = SecretReference::new(disallowed).expect("port-level reference");
            assert!(matches!(
                provider.validate_reference(&reference),
                Err(SecretProviderValidationError::ReferenceOutOfScope)
            ));
        }
    }

    #[test]
    fn secure_string_parameter_decodes_without_exposing_debug_output() {
        let reference =
            SecretReference::new("/novus/development/model/key").expect("valid reference");
        let parameter = aws_sdk_ssm::types::Parameter::builder()
            .name(reference.as_str())
            .r#type(ParameterType::SecureString)
            .value("sensitive-test-value")
            .build();
        let secret = SsmSecretProvider::decode_parameter(&reference, parameter)
            .expect("SecureString should decode");
        assert_eq!(secret.expose(), b"sensitive-test-value");
        assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");
    }

    #[test]
    fn non_secure_missing_and_oversized_parameters_fail_closed() {
        let reference =
            SecretReference::new("/novus/development/model/key").expect("valid reference");
        let plain = aws_sdk_ssm::types::Parameter::builder()
            .name(reference.as_str())
            .r#type(ParameterType::String)
            .value("plain-text")
            .build();
        assert!(matches!(
            SsmSecretProvider::decode_parameter(&reference, plain),
            Err(SecretProviderError::Validation(
                SecretProviderValidationError::ParameterNotSecureString
            ))
        ));

        let missing = aws_sdk_ssm::types::Parameter::builder()
            .name(reference.as_str())
            .r#type(ParameterType::SecureString)
            .build();
        assert!(matches!(
            SsmSecretProvider::decode_parameter(&reference, missing),
            Err(SecretProviderError::Validation(
                SecretProviderValidationError::ParameterValueMissing
            ))
        ));

        let empty = aws_sdk_ssm::types::Parameter::builder()
            .name(reference.as_str())
            .r#type(ParameterType::SecureString)
            .value("")
            .build();
        assert!(matches!(
            SsmSecretProvider::decode_parameter(&reference, empty),
            Err(SecretProviderError::SecretValue)
        ));

        let wrong_name = aws_sdk_ssm::types::Parameter::builder()
            .name("/novus/development/model/other-key")
            .r#type(ParameterType::SecureString)
            .value("sensitive-test-value")
            .build();
        assert!(matches!(
            SsmSecretProvider::decode_parameter(&reference, wrong_name),
            Err(SecretProviderError::Validation(
                SecretProviderValidationError::ParameterNameMismatch
            ))
        ));

        let oversized = aws_sdk_ssm::types::Parameter::builder()
            .name(reference.as_str())
            .r#type(ParameterType::SecureString)
            .value("x".repeat(MAX_STANDARD_SECURE_STRING_BYTES + 1))
            .build();
        assert!(matches!(
            SsmSecretProvider::decode_parameter(&reference, oversized),
            Err(SecretProviderError::Validation(
                SecretProviderValidationError::ParameterValueTooLarge
            ))
        ));
    }

    #[test]
    fn errors_and_provider_debug_are_redaction_safe() {
        let provider = SsmSecretProvider::new(client(), "development").expect("valid provider");
        let debug = format!("{provider:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sensitive-test-value"));

        let error = SecretProviderError::Service {
            operation: "get parameter",
        };
        assert_eq!(error.to_string(), "Parameter Store get parameter failed");
    }
}
