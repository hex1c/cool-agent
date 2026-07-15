#![allow(clippy::expect_used)]

use application::config::Environment;
use application::config_loader::{ConfigError, load_config};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/config")
        .join(name)
}

fn repository_file(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name)
}

#[test]
fn config_loader_validates_all_environment_overrides() {
    let schema = repository_file("config/schema/company.schema.json");
    for (file, environment) in [
        ("dev.yaml", Environment::Development),
        ("staging.yaml", Environment::Staging),
        ("production.yaml", Environment::Production),
    ] {
        let config = application::config_loader::load_config(
            repository_file("config/company.yaml"),
            repository_file(&format!("config/environments/{file}")),
            schema.clone(),
            environment.clone(),
        )
        .expect("versioned environment config should load");
        assert_eq!(config.environment, environment);
    }
}

#[test]
fn config_loader_rejects_invalid_budget_values() {
    let error = application::config_loader::load_config(
        fixture("company.yaml"),
        fixture("invalid.yaml"),
        repository_file("config/schema/company.schema.json"),
        Environment::Development,
    )
    .expect_err("invalid budget must be rejected");

    assert!(matches!(
        error,
        ConfigError::SchemaViolation { .. } | ConfigError::InvalidValue { .. }
    ));
}

#[test]
fn config_loader_rejects_cross_environment_overrides() {
    let error = application::config_loader::load_config(
        fixture("company.yaml"),
        fixture("development.yaml"),
        repository_file("config/schema/company.schema.json"),
        Environment::Production,
    )
    .expect_err("development config must not load as production");

    assert!(matches!(error, ConfigError::EnvironmentMismatch { .. }));
}

#[test]
fn config_loader_rejects_raw_secret_values() {
    let error = load_config(
        fixture("company.yaml"),
        fixture("invalid-secret.yaml"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/schema/company.schema.json"),
        Environment::Development,
    )
    .expect_err("raw secret must be rejected");

    assert!(matches!(
        error,
        ConfigError::SchemaViolation { .. } | ConfigError::InvalidValue { .. }
    ));
}
