use domain::{
    BudgetAction, BudgetAuthorization, BudgetBand, BudgetDenialReason, BudgetEvaluation,
    BudgetPolicyError, BudgetThresholds, ConfirmedOperationFunding, apply_safety_margin,
    evaluate_budget,
};

const WARNING: u64 = 240_000_000;
const SUSPENSION: u64 = 270_000_000;
const HARD_CAP: u64 = 300_000_000;
const OPERATIONAL_RESERVE: u64 = 10_000_000;
const WORKFLOW_SPEND_LIMIT: u64 = HARD_CAP - OPERATIONAL_RESERVE;

fn thresholds() -> Result<BudgetThresholds, BudgetPolicyError> {
    BudgetThresholds::new(WARNING, SUSPENSION, HARD_CAP, OPERATIONAL_RESERVE)
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

fn confirmed(funding: ConfirmedOperationFunding) -> BudgetAction {
    BudgetAction::ConfirmedOperation { funding }
}

#[test]
fn safety_margin_uses_integer_ceiling_arithmetic_at_configured_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(apply_safety_margin(200_000_000, 2_000)?, WARNING);
    assert_eq!(apply_safety_margin(225_000_000, 2_000)?, SUSPENSION);
    assert_eq!(apply_safety_margin(250_000_000, 2_000)?, HARD_CAP);
    assert_eq!(apply_safety_margin(1, 1)?, 2);
    assert_eq!(apply_safety_margin(0, 2_000)?, 0);
    Ok(())
}

#[test]
fn safety_margin_reports_projection_overflow() {
    assert_eq!(
        apply_safety_margin(u64::MAX, 1),
        Err(BudgetPolicyError::ProjectionOverflow)
    );
    assert_eq!(apply_safety_margin(u64::MAX, 0), Ok(u64::MAX));
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

    let already_warned = decision(
        confirmed(ConfirmedOperationFunding::ExistingReservation),
        WARNING,
        WARNING + 1,
    )?;
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
fn new_workflow_reservations_cannot_consume_the_operational_reserve()
-> Result<(), Box<dyn std::error::Error>> {
    let at_limit = decision(
        confirmed(ConfirmedOperationFunding::RequiresNewReservation),
        WORKFLOW_SPEND_LIMIT - 1,
        WORKFLOW_SPEND_LIMIT,
    )?;
    assert_eq!(at_limit.band(), BudgetBand::IntakeSuspended);
    assert_eq!(at_limit.authorization(), BudgetAuthorization::Permit);

    let over_limit = decision(
        confirmed(ConfirmedOperationFunding::RequiresNewReservation),
        WORKFLOW_SPEND_LIMIT,
        WORKFLOW_SPEND_LIMIT + 1,
    )?;
    assert_eq!(over_limit.band(), BudgetBand::OperationalReserve);
    assert_eq!(
        over_limit.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::ProtectedOperationalReserve)
    );

    let intake_in_reserve = decision(
        BudgetAction::NewWorkflowIntake,
        WORKFLOW_SPEND_LIMIT,
        WORKFLOW_SPEND_LIMIT + 1,
    )?;
    assert_eq!(
        intake_in_reserve.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
    );

    let deployment_in_reserve = decision(
        BudgetAction::Deployment,
        WORKFLOW_SPEND_LIMIT,
        WORKFLOW_SPEND_LIMIT + 1,
    )?;
    assert_eq!(
        deployment_in_reserve.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::DeploymentWhileIntakeSuspended)
    );
    Ok(())
}

#[test]
fn existing_confirmed_reservations_may_finish_below_the_hard_cap()
-> Result<(), Box<dyn std::error::Error>> {
    let result = decision(
        confirmed(ConfirmedOperationFunding::ExistingReservation),
        WORKFLOW_SPEND_LIMIT,
        HARD_CAP - 1,
    )?;
    assert_eq!(result.band(), BudgetBand::OperationalReserve);
    assert_eq!(result.authorization(), BudgetAuthorization::Permit);

    let at_cap = decision(
        confirmed(ConfirmedOperationFunding::ExistingReservation),
        HARD_CAP - 1,
        HARD_CAP,
    )?;
    assert_eq!(at_cap.band(), BudgetBand::HardCap);
    assert_eq!(
        at_cap.authorization(),
        BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
    );
    Ok(())
}

#[test]
fn bounded_operational_actions_may_use_the_reserve_but_not_reach_the_cap()
-> Result<(), Box<dyn std::error::Error>> {
    for action in [
        BudgetAction::Status,
        BudgetAction::Cancellation,
        BudgetAction::FailureReporting,
        BudgetAction::ArtifactRetrieval,
    ] {
        let within_reserve = decision(action, WORKFLOW_SPEND_LIMIT, HARD_CAP - 1)?;
        assert_eq!(within_reserve.band(), BudgetBand::OperationalReserve);
        assert_eq!(within_reserve.authorization(), BudgetAuthorization::Permit);

        let at_cap = decision(action, HARD_CAP - 1, HARD_CAP)?;
        assert_eq!(at_cap.band(), BudgetBand::HardCap);
        assert_eq!(
            at_cap.authorization(),
            BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached)
        );
    }
    Ok(())
}

#[test]
fn policy_rejects_invalid_thresholds_reserve_and_decreasing_projection() {
    assert_eq!(
        BudgetThresholds::new(WARNING, WARNING, HARD_CAP, OPERATIONAL_RESERVE),
        Err(BudgetPolicyError::InvalidThresholdOrder)
    );
    assert_eq!(
        BudgetThresholds::new(WARNING, SUSPENSION, HARD_CAP, HARD_CAP),
        Err(BudgetPolicyError::InvalidOperationalReserve)
    );
    assert_eq!(
        BudgetThresholds::new(WARNING, SUSPENSION, HARD_CAP, 30_000_000),
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
