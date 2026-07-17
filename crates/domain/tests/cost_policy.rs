use domain::{
    BudgetAction, BudgetAuthorization, BudgetBand, BudgetDenialReason, BudgetEvaluation,
    BudgetPolicyError, BudgetThresholds, evaluate_budget,
};

const WARNING: u64 = 240_000_000;
const SUSPENSION: u64 = 270_000_000;
const HARD_CAP: u64 = 300_000_000;

fn thresholds() -> Result<BudgetThresholds, BudgetPolicyError> {
    BudgetThresholds::new(WARNING, SUSPENSION, HARD_CAP)
}

fn decision(
    action: BudgetAction,
    current: u64,
    projected_after_action: u64,
) -> Result<domain::BudgetDecision, BudgetPolicyError> {
    Ok(evaluate_budget(
        thresholds()?,
        BudgetEvaluation::new(action, current, projected_after_action)?,
    ))
}

#[test]
fn configured_warning_boundary_is_inclusive_and_reports_only_a_crossing()
-> Result<(), Box<dyn std::error::Error>> {
    let below = decision(BudgetAction::NewWorkflowIntake, 0, WARNING - 1)?;
    assert_eq!(below.band(), BudgetBand::Normal);
    assert_eq!(below.authorization(), BudgetAuthorization::Permit);
    assert!(!below.warning_crossed());

    let crossing = decision(BudgetAction::NewWorkflowIntake, WARNING - 1, WARNING)?;
    assert_eq!(crossing.band(), BudgetBand::Warning);
    assert_eq!(crossing.authorization(), BudgetAuthorization::Permit);
    assert!(crossing.warning_crossed());

    let already_warned = decision(BudgetAction::ConfirmedOperation, WARNING, WARNING + 1)?;
    assert_eq!(already_warned.band(), BudgetBand::Warning);
    assert_eq!(already_warned.authorization(), BudgetAuthorization::Permit);
    assert!(!already_warned.warning_crossed());
    Ok(())
}

#[test]
fn intake_and_deployment_stop_at_the_inclusive_suspension_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let intake_below = decision(
        BudgetAction::NewWorkflowIntake,
        SUSPENSION - 2,
        SUSPENSION - 1,
    )?;
    assert_eq!(intake_below.authorization(), BudgetAuthorization::Permit);

    let intake_at = decision(BudgetAction::NewWorkflowIntake, SUSPENSION - 1, SUSPENSION)?;
    assert_eq!(intake_at.band(), BudgetBand::IntakeSuspended);
    assert_eq!(
        intake_at.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
    );

    let deployment_at = decision(BudgetAction::Deployment, SUSPENSION, SUSPENSION)?;
    assert_eq!(
        deployment_at.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::DeploymentWhileIntakeSuspended)
    );
    Ok(())
}

#[test]
fn confirmed_work_proceeds_during_suspension_only_while_remaining_below_the_cap()
-> Result<(), Box<dyn std::error::Error>> {
    let at_suspension = decision(BudgetAction::ConfirmedOperation, SUSPENSION - 1, SUSPENSION)?;
    assert_eq!(at_suspension.authorization(), BudgetAuthorization::Permit);

    let below_cap = decision(BudgetAction::ConfirmedOperation, HARD_CAP - 2, HARD_CAP - 1)?;
    assert_eq!(below_cap.authorization(), BudgetAuthorization::Permit);

    let at_cap = decision(BudgetAction::ConfirmedOperation, HARD_CAP - 1, HARD_CAP)?;
    assert_eq!(at_cap.band(), BudgetBand::HardCap);
    assert_eq!(
        at_cap.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
    );
    Ok(())
}

#[test]
fn status_and_artifact_retrieval_remain_available_during_intake_suspension()
-> Result<(), Box<dyn std::error::Error>> {
    for action in [BudgetAction::Status, BudgetAction::ArtifactRetrieval] {
        let result = decision(action, SUSPENSION, SUSPENSION)?;
        assert_eq!(result.band(), BudgetBand::IntakeSuspended);
        assert_eq!(result.authorization(), BudgetAuthorization::Permit);
    }
    Ok(())
}

#[test]
fn every_action_fails_closed_at_the_hard_cap() -> Result<(), Box<dyn std::error::Error>> {
    for action in [
        BudgetAction::NewWorkflowIntake,
        BudgetAction::ConfirmedOperation,
        BudgetAction::Status,
        BudgetAction::ArtifactRetrieval,
        BudgetAction::Deployment,
    ] {
        let result = decision(action, HARD_CAP, HARD_CAP)?;
        assert_eq!(result.band(), BudgetBand::HardCap);
        assert_eq!(
            result.authorization(),
            BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
        );
    }
    Ok(())
}

#[test]
fn policy_rejects_invalid_threshold_order_and_decreasing_projection() {
    assert_eq!(
        BudgetThresholds::new(WARNING, WARNING, HARD_CAP),
        Err(BudgetPolicyError::InvalidThresholdOrder)
    );
    assert_eq!(
        BudgetThresholds::new(WARNING, SUSPENSION, SUSPENSION),
        Err(BudgetPolicyError::InvalidThresholdOrder)
    );
    assert_eq!(
        BudgetEvaluation::new(BudgetAction::Status, 1, 0),
        Err(BudgetPolicyError::ProjectionDecreased {
            current_micro_inr: 1,
            projected_after_action_micro_inr: 0,
        })
    );
}
