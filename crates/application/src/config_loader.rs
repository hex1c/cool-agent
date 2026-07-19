use crate::config::{BudgetConfig, DeploymentConfig, Environment};
use jsonschema::Validator;
use serde_json::Value;
use std::fs;
use std::path::Path;
use thiserror::Error;

const EXPECTED_SCHEMA_VERSION: &str = "novus.config.v1";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid YAML in {path}: {source}")]
    Yaml {
        path: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("invalid JSON schema in {path}: {source}")]
    SchemaFile {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("configuration schema rejected {path}: {message}")]
    SchemaViolation { path: String, message: String },
    #[error("configuration is not complete or typed correctly: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("configuration field {field} is invalid: {reason}")]
    InvalidValue { field: String, reason: String },
    #[error("environment override is for {actual}, but {expected} was requested")]
    EnvironmentMismatch { expected: String, actual: String },
}

pub fn load_config(
    base_path: impl AsRef<Path>,
    environment_path: impl AsRef<Path>,
    schema_path: impl AsRef<Path>,
    expected_environment: Environment,
) -> Result<DeploymentConfig, ConfigError> {
    let base_path = base_path.as_ref();
    let environment_path = environment_path.as_ref();
    let schema_path = schema_path.as_ref();
    let schema = read_json(schema_path)?;
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .map_err(|error| ConfigError::SchemaViolation {
            path: schema_path.display().to_string(),
            message: error.to_string(),
        })?;

    let base = read_yaml(base_path)?;
    validate(&validator, base_path, &base)?;
    let environment = read_yaml(environment_path)?;
    validate(&validator, environment_path, &environment)?;
    let expected_name = environment_name(&expected_environment);
    let actual_name = environment
        .get("environment")
        .and_then(Value::as_str)
        .ok_or_else(|| ConfigError::InvalidValue {
            field: "environment".to_owned(),
            reason: "an environment override must declare its environment".to_owned(),
        })?;
    if actual_name != expected_name {
        return Err(ConfigError::EnvironmentMismatch {
            expected: expected_name.to_owned(),
            actual: actual_name.to_owned(),
        });
    }

    let mut merged = base;
    merge_objects(&mut merged, environment);
    set_object_value(
        &mut merged,
        "environment",
        Value::String(expected_name.to_owned()),
    );
    remove_object_value(&mut merged, "kind");
    validate(&validator, base_path, &merged)?;

    let config: DeploymentConfig = serde_json::from_value(merged)?;
    validate_semantics(&config, expected_name)
}

fn read_yaml(path: &Path) -> Result<Value, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_yaml::from_str(&contents).map_err(|source| ConfigError::Yaml {
        path: path.display().to_string(),
        source,
    })
}

fn read_json(path: &Path) -> Result<Value, ConfigError> {
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&contents).map_err(|source| ConfigError::SchemaFile {
        path: path.display().to_string(),
        source,
    })
}

fn validate(validator: &Validator, path: &Path, value: &Value) -> Result<(), ConfigError> {
    if let Some(error) = validator.iter_errors(value).next() {
        return Err(ConfigError::SchemaViolation {
            path: path.display().to_string(),
            message: error.to_string(),
        });
    }
    Ok(())
}

fn merge_objects(target: &mut Value, overlay: Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay {
                if key == "schemaVersion" || key == "kind" {
                    continue;
                }
                match target.get_mut(&key) {
                    Some(existing) => merge_objects(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, overlay) => *target = overlay,
    }
}

fn set_object_value(root: &mut Value, key: &str, value: Value) {
    if let Value::Object(object) = root {
        object.insert(key.to_owned(), value);
    }
}

fn remove_object_value(root: &mut Value, key: &str) {
    if let Value::Object(object) = root {
        object.remove(key);
    }
}

fn environment_name(environment: &Environment) -> &'static str {
    match environment {
        Environment::Development => "development",
        Environment::Staging => "staging",
        Environment::Production => "production",
    }
}

fn validate_semantics(
    config: &DeploymentConfig,
    expected_environment: &str,
) -> Result<DeploymentConfig, ConfigError> {
    if config.schema_version != EXPECTED_SCHEMA_VERSION {
        return Err(ConfigError::InvalidValue {
            field: "schemaVersion".to_owned(),
            reason: format!("expected {EXPECTED_SCHEMA_VERSION}"),
        });
    }
    validate_budget(&config.budget)?;
    validate_normalization(&config.normalization)?;
    if config.retry.max_attempts == 0 || config.retry.max_attempts > 3 {
        return Err(ConfigError::InvalidValue {
            field: "retry.maxAttempts".to_owned(),
            reason: "must be between 1 and 3".to_owned(),
        });
    }
    if config.attachments.max_count == 0 || config.attachments.max_count > 10 {
        return Err(ConfigError::InvalidValue {
            field: "attachments.maxCount".to_owned(),
            reason: "must be between 1 and 10".to_owned(),
        });
    }
    if config.attachments.max_bytes == 0 || config.attachments.max_bytes > 20 * 1024 * 1024 {
        return Err(ConfigError::InvalidValue {
            field: "attachments.maxBytes".to_owned(),
            reason: "must be between 1 byte and 20 MiB".to_owned(),
        });
    }
    for (field, value) in [
        ("email.secretRef", &config.email.secret_ref),
        ("ai.secretRef", &config.ai.secret_ref),
        ("telegram.botTokenRef", &config.telegram.bot_token_ref),
        (
            "telegram.webhookSecretRef",
            &config.telegram.webhook_secret_ref,
        ),
        ("google.clientSecretRef", &config.google.client_secret_ref),
    ] {
        let expected_prefix = format!("/novus/{expected_environment}/");
        if !value.starts_with(&expected_prefix) {
            return Err(ConfigError::InvalidValue {
                field: field.to_owned(),
                reason: format!("must start with {expected_prefix}"),
            });
        }
    }
    Ok(config.clone())
}

fn validate_normalization(config: &crate::config::NormalizationConfig) -> Result<(), ConfigError> {
    if config.max_image_pixels == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxImagePixels".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_pdf_pages == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxPdfPages".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_decompressed_bytes == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxDecompressedBytes".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_normalized_text_bytes == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxNormalizedTextBytes".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_csv_rows == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxCsvRows".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_csv_cells == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxCsvCells".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_office_uncompressed_bytes == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxOfficeUncompressedBytes".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    if config.max_compression_ratio == 0 {
        return Err(ConfigError::InvalidValue {
            field: "normalization.maxCompressionRatio".to_owned(),
            reason: "must be non-zero".to_owned(),
        });
    }
    Ok(())
}

fn validate_budget(budget: &BudgetConfig) -> Result<(), ConfigError> {
    if budget.warning_threshold_micro_inr >= budget.suspension_threshold_micro_inr
        || budget.suspension_threshold_micro_inr >= budget.monthly_cap_micro_inr
    {
        return Err(ConfigError::InvalidValue {
            field: "budget".to_owned(),
            reason: "warning < suspension < monthly cap is required".to_owned(),
        });
    }
    if budget.operational_reserve_micro_inr >= budget.monthly_cap_micro_inr {
        return Err(ConfigError::InvalidValue {
            field: "budget.operationalReserveMicroInr".to_owned(),
            reason: "must be lower than the monthly cap".to_owned(),
        });
    }
    let workflow_spend_limit = budget.monthly_cap_micro_inr - budget.operational_reserve_micro_inr;
    if budget.suspension_threshold_micro_inr >= workflow_spend_limit {
        return Err(ConfigError::InvalidValue {
            field: "budget.operationalReserveMicroInr".to_owned(),
            reason: "must leave workflow spend capacity above the suspension threshold".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::load_config;
    use crate::config::Environment;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/config")
            .join(name)
    }

    #[test]
    fn loads_and_merges_environment_overrides() {
        let config = load_config(
            fixture("company.yaml"),
            fixture("development.yaml"),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../config/schema/company.schema.json"),
            Environment::Development,
        )
        .expect("fixture config should load");

        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.telegram.bot_username, "novus_dev_bot");
        assert_eq!(config.budget.monthly_cap_micro_inr, 300_000_000);
    }
}
