#![allow(clippy::expect_used)]

use std::collections::HashSet;
use std::path::PathBuf;

use application::ports::{
    ObjectClass, PageToken, PortValueError, SecretReference, SecretValue, StorageKey,
    StorageRecordId, StoredObject,
};
use application::repositories::{
    HistoryCheckpoint, HistorySequence, InvoiceMonth, MAX_PAGE_SIZE, PageRequest,
    RepositoryValueError,
};
use domain::WorkflowTimestamp;
use domain::identity::WorkflowId;
use serde::Deserialize;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/storage")
        .join(name)
}

fn workflow(value: &str) -> WorkflowId {
    WorkflowId::new(value).expect("test workflow id must be valid")
}

fn stored_object(workflow_id: WorkflowId, class: ObjectClass, key: &str) -> StoredObject {
    StoredObject {
        workflow_id,
        object_id: StorageRecordId::new("object-1").expect("test object id must be valid"),
        class,
        storage_key: StorageKey::new(key).expect("test storage key must be valid"),
        byte_length: 42,
        sha256: [7; 32],
        media_type: "application/json".to_owned(),
        created_at: WorkflowTimestamp::from_unix_seconds(100),
    }
}

#[test]
fn persistence_identifiers_reject_ambiguous_or_control_bearing_values() {
    assert_eq!(
        StorageRecordId::new(""),
        Err(PortValueError::Empty {
            kind: "storage record id"
        })
    );
    assert!(matches!(
        StorageRecordId::new("has/slash"),
        Err(PortValueError::InvalidCharacters { .. })
    ));
    assert!(matches!(
        StorageRecordId::new("a".repeat(129)),
        Err(PortValueError::TooLong { maximum: 128, .. })
    ));
    assert!(StorageRecordId::new("record-17_valid").is_ok());
}

#[test]
fn object_contract_enforces_environment_bound_prefix_classes() {
    let valid = stored_object(
        workflow("workflow-1"),
        ObjectClass::SanitizedHistory,
        "history/workflow-1/00000000000000000001.json",
    );
    assert_eq!(valid.validate(), Ok(()));

    let wrong_prefix = StoredObject {
        class: ObjectClass::RawInput,
        ..valid.clone()
    };
    assert!(matches!(
        wrong_prefix.validate(),
        Err(PortValueError::InvalidCharacters { .. })
    ));

    let invalid_media_type = StoredObject {
        media_type: String::new(),
        ..valid
    };
    assert!(matches!(
        invalid_media_type.validate(),
        Err(PortValueError::Empty { kind: "media type" })
    ));

    let cross_workflow = stored_object(
        workflow("workflow-2"),
        ObjectClass::SanitizedHistory,
        "history/workflow-1/00000000000000000001.json",
    );
    assert!(matches!(
        cross_workflow.validate(),
        Err(PortValueError::InvalidCharacters { .. })
    ));
    assert!(StorageKey::new("other/workflow-1/value").is_err());
    assert!(StorageKey::new("raw/").is_err());
}

#[test]
fn secret_values_are_redacted_and_references_are_environment_scoped() {
    let secret = SecretValue::new(b"sensitive-test-value".to_vec())
        .expect("non-empty secret should be accepted");
    assert_eq!(format!("{secret:?}"), "SecretValue([REDACTED])");
    assert_eq!(secret.expose(), b"sensitive-test-value");

    assert!(SecretReference::new("/novus/development/google/client-secret").is_ok());
    assert!(matches!(
        SecretReference::new("raw-secret-value"),
        Err(PortValueError::InvalidSecretReference)
    ));
    assert!(SecretReference::new("/novus/development/bad path").is_err());
}

#[test]
fn pagination_and_invoice_month_values_are_bounded() {
    assert!(PageRequest::new(1, None).is_ok());
    assert!(PageRequest::new(MAX_PAGE_SIZE, PageToken::new("next").ok()).is_ok());
    assert_eq!(
        PageRequest::new(0, None),
        Err(RepositoryValueError::InvalidPageSize { requested: 0 })
    );
    assert_eq!(
        PageRequest::new(MAX_PAGE_SIZE + 1, None),
        Err(RepositoryValueError::InvalidPageSize {
            requested: MAX_PAGE_SIZE + 1
        })
    );
    assert_eq!(
        PageToken::new("a".repeat(2_049)),
        Err(PortValueError::TooLong {
            kind: "page token",
            maximum: 2_048
        })
    );

    assert_eq!(
        InvoiceMonth::new("2026-07").map(|month| month.as_str().to_owned()),
        Ok("2026-07".to_owned())
    );
    for invalid in ["2026-00", "2026-13", "26-07", "2026/07", "abcd-07"] {
        assert_eq!(
            InvoiceMonth::new(invalid),
            Err(RepositoryValueError::InvalidInvoiceMonth)
        );
    }
}

#[test]
fn history_checkpoint_binds_ordered_metadata_to_one_workflow()
-> Result<(), Box<dyn std::error::Error>> {
    let checkpoint = HistoryCheckpoint {
        workflow_id: workflow("workflow-1"),
        sequence: HistorySequence::new(9),
        object: stored_object(
            workflow("workflow-1"),
            ObjectClass::SanitizedHistory,
            "history/workflow-1/00000000000000000009.json",
        ),
        model_version: "model-v1".to_owned(),
        prompt_version: "prompt-v1".to_owned(),
        created_at: WorkflowTimestamp::from_unix_seconds(100),
    };
    assert_eq!(checkpoint.sequence.get(), 9);
    assert_eq!(checkpoint.validate(), Ok(()));

    let mismatched = HistoryCheckpoint {
        workflow_id: workflow("workflow-2"),
        ..checkpoint.clone()
    };
    assert!(matches!(
        mismatched.validate(),
        Err(RepositoryValueError::Port(
            PortValueError::InvalidCharacters { .. }
        ))
    ));

    let raw_history = HistoryCheckpoint {
        object: stored_object(
            workflow("workflow-1"),
            ObjectClass::RawInput,
            "raw/workflow-1/source.json",
        ),
        ..checkpoint
    };
    assert!(matches!(
        raw_history.validate(),
        Err(RepositoryValueError::Port(
            PortValueError::InvalidCharacters { .. }
        ))
    ));
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFixture {
    schema_version: String,
    cases: Vec<KeyCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyCase {
    name: String,
    environment: String,
    table: String,
    entity: String,
    pk: String,
    sk: String,
    ttl_cleanup_only: bool,
}

#[test]
fn key_fixture_proves_environment_isolation_ordering_and_cleanup_only_ttl()
-> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(fixture("key-cases.json"))?;
    let fixture: KeyFixture = serde_json::from_slice(&bytes)?;
    assert_eq!(fixture.schema_version, "novus.storage-keys.v1");

    let mut physical_keys = HashSet::new();
    for case in &fixture.cases {
        assert!(!case.name.is_empty());
        assert!(matches!(
            case.environment.as_str(),
            "development" | "production"
        ));
        assert!(matches!(case.table.as_str(), "application" | "budget"));
        assert!(physical_keys.insert((
            case.environment.as_str(),
            case.table.as_str(),
            case.pk.as_str(),
            case.sk.as_str(),
        )));

        if case.entity == "history_pointer" {
            assert!(case.sk.starts_with("HISTORY#00000000000000000009"));
            assert!(!case.ttl_cleanup_only);
        }
        if matches!(case.entity.as_str(), "oauth_state" | "membership_cache") {
            assert!(case.ttl_cleanup_only);
        }
    }

    let actual_entities: HashSet<_> = fixture
        .cases
        .iter()
        .map(|case| case.entity.as_str())
        .collect();
    let expected_entities = HashSet::from([
        "workflow",
        "topic_claim",
        "audit",
        "confirmation",
        "operation_journal",
        "history_pointer",
        "object_metadata",
        "oauth_state",
        "membership_cache",
        "budget_aggregate",
        "budget_reservation",
        "pricing_decision",
        "reconciliation",
    ]);
    assert_eq!(actual_entities, expected_entities);

    let isolated_same_key_count = fixture
        .cases
        .iter()
        .filter(|case| case.entity == "workflow" && case.pk == "WF#workflow-17")
        .count();
    assert_eq!(isolated_same_key_count, 2);
    Ok(())
}
