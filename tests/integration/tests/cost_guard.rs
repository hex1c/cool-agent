#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root exists")
}

#[test]
fn cost_observability_monitoring_covers_required_failure_and_threshold_signals() {
    let root = repository_root();
    let monitoring = fs::read_to_string(root.join("infrastructure/monitoring.yaml"))
        .expect("monitoring template exists");
    let template =
        fs::read_to_string(root.join("infrastructure/template.yaml")).expect("SAM template exists");

    for required in [
        "BudgetWarningAlarm:",
        "BudgetSuspensionAlarm:",
        "BudgetHardCapAlarm:",
        "BudgetMetricStaleAlarm:",
        "CostGuardFailureAlarm:",
        "ManualReviewAlarm:",
        "QuotationExecutionFailureAlarm:",
        "CalendarExecutionFailureAlarm:",
        "EmailExecutionFailureAlarm:",
        "WebhookErrorRateAlarm:",
        "OAuthErrorRateAlarm:",
    ] {
        assert!(monitoring.contains(required), "missing {required}");
    }

    assert!(monitoring.contains("MetricName: ProjectedMonthlyCostMicroInr"));
    assert!(monitoring.contains("Threshold: 240000000"));
    assert!(monitoring.contains("Threshold: 270000000"));
    assert!(monitoring.contains("Threshold: 300000000"));
    assert_eq!(monitoring.matches("TreatMissingData: breaching").count(), 1);
    assert!(monitoring.contains("Namespace: Novus/Edge"));
    assert!(monitoring.contains("MetricName: WebhookErrorCount"));
    assert!(monitoring.contains("MetricName: OAuthErrorCount"));
    assert!(monitoring.contains("100*webhookErrors/webhookInvocations"));
    assert!(monitoring.contains("100*oauthErrors/oauthInvocations"));
    assert!(template.contains("MonitoringStack:"));
    assert!(template.contains("Location: monitoring.yaml"));
    assert!(template.contains("CostGuardIntakeFunction:"));
    assert!(template.contains("CostGuardAiFunction:"));
    assert!(template.contains("CostGuardExternalFunction:"));
    assert!(template.contains("BudgetHeartbeat:"));
    assert!(template.contains("IncludeExecutionData: false"));
}

#[test]
fn sam_build_stages_only_the_infrastructure_build_driver() {
    let root = repository_root();
    let template =
        fs::read_to_string(root.join("infrastructure/template.yaml")).expect("SAM template exists");
    let build_driver =
        fs::read_to_string(root.join("infrastructure/Makefile")).expect("build driver exists");

    assert_eq!(template.matches("      CodeUri: .\n").count(), 14);
    assert!(!template.contains("      CodeUri: ..\n"));
    assert!(build_driver.contains("ARTIFACTS_DIR)/../../.."));
    assert!(build_driver.contains("$(REPO_ROOT)/functions/workflow-actions"));
}

#[test]
fn cost_observability_budget_writes_remain_cost_guard_only() {
    let policies =
        fs::read_to_string(repository_root().join("infrastructure/policies/functions.yaml"))
            .expect("IAM policy template exists");

    assert!(policies.contains("- dynamodb:TransactWriteItems"));
    assert_eq!(
        policies.matches("- !Ref DynamoDBBudgetWritePolicy").count(),
        1,
        "only the environment-bound cost guard role may write budget counters"
    );
    let role_start = policies.find("  CostGuardRole:").expect("cost guard role");
    let role_end = policies[role_start..]
        .find("\n  AgentHarnessRole:")
        .map(|offset| role_start + offset)
        .expect("next role boundary");
    assert!(policies[role_start..role_end].contains("- !Ref DynamoDBBudgetWritePolicy"));
}
