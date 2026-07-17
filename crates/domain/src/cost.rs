use std::fmt::{Display, Formatter};

const BASIS_POINTS: u128 = 10_000;

/// Validated per-environment budget boundaries, expressed in integer micro-INR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetThresholds {
    warning_micro_inr: u64,
    suspension_micro_inr: u64,
    workflow_spend_limit_micro_inr: u64,
    hard_cap_micro_inr: u64,
}

impl BudgetThresholds {
    pub fn new(
        warning_micro_inr: u64,
        suspension_micro_inr: u64,
        hard_cap_micro_inr: u64,
        operational_reserve_micro_inr: u64,
    ) -> Result<Self, BudgetPolicyError> {
        if operational_reserve_micro_inr >= hard_cap_micro_inr {
            return Err(BudgetPolicyError::InvalidOperationalReserve);
        }
        let workflow_spend_limit_micro_inr = hard_cap_micro_inr - operational_reserve_micro_inr;
        if warning_micro_inr >= suspension_micro_inr
            || suspension_micro_inr >= workflow_spend_limit_micro_inr
        {
            return Err(BudgetPolicyError::InvalidThresholdOrder);
        }
        Ok(Self {
            warning_micro_inr,
            suspension_micro_inr,
            workflow_spend_limit_micro_inr,
            hard_cap_micro_inr,
        })
    }

    pub const fn workflow_spend_limit_micro_inr(self) -> u64 {
        self.workflow_spend_limit_micro_inr
    }
}

/// Whether confirmed work already owns cost capacity or needs a new reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmedOperationFunding {
    ExistingReservation,
    RequiresNewReservation,
}

/// The operation for which the caller needs a budget authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAction {
    NewWorkflowIntake,
    ConfirmedOperation { funding: ConfirmedOperationFunding },
    Status,
    Cancellation,
    FailureReporting,
    ArtifactRetrieval,
    Deployment,
}

/// Trusted effective projections before and after one action.
///
/// The values must already include reservations and reconciliation tightening.
/// Use [`apply_safety_margin`] on estimates before constructing this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetEvaluation {
    action: BudgetAction,
    current_effective_projected_micro_inr: u64,
    projected_after_action_micro_inr: u64,
}

impl BudgetEvaluation {
    pub fn new(
        action: BudgetAction,
        current_effective_projected_micro_inr: u64,
        projected_after_action_micro_inr: u64,
    ) -> Result<Self, BudgetPolicyError> {
        if projected_after_action_micro_inr < current_effective_projected_micro_inr {
            return Err(BudgetPolicyError::ProjectionDecreased {
                current_micro_inr: current_effective_projected_micro_inr,
                projected_after_action_micro_inr,
            });
        }
        Ok(Self {
            action,
            current_effective_projected_micro_inr,
            projected_after_action_micro_inr,
        })
    }
}

/// Threshold band reached by the post-action effective projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetBand {
    Normal,
    Warning,
    IntakeSuspended,
    OperationalReserve,
    HardCap,
}

/// Why a requested action was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDenialReason {
    IntakeSuspended,
    DeploymentWhileIntakeSuspended,
    ProtectedOperationalReserve,
    HardCapReached,
}

/// Whether the budget policy permits the action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAuthorization {
    Permit,
    Deny(BudgetDenialReason),
}

/// Pure threshold and authorization result for one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetDecision {
    band: BudgetBand,
    authorization: BudgetAuthorization,
    warning_crossed: bool,
    projected_after_action_micro_inr: u64,
}

impl BudgetDecision {
    pub const fn band(self) -> BudgetBand {
        self.band
    }

    pub const fn authorization(self) -> BudgetAuthorization {
        self.authorization
    }

    pub const fn warning_crossed(self) -> bool {
        self.warning_crossed
    }

    pub const fn projected_after_action_micro_inr(self) -> u64 {
        self.projected_after_action_micro_inr
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPolicyError {
    InvalidThresholdOrder,
    InvalidOperationalReserve,
    ProjectionDecreased {
        current_micro_inr: u64,
        projected_after_action_micro_inr: u64,
    },
    ProjectionOverflow,
}

impl Display for BudgetPolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidThresholdOrder => formatter.write_str(
                "budget thresholds must be ordered warning < suspension < workflow spend limit < hard cap",
            ),
            Self::InvalidOperationalReserve => {
                formatter.write_str("operational reserve must be lower than the hard cap")
            }
            Self::ProjectionDecreased {
                current_micro_inr,
                projected_after_action_micro_inr,
            } => write!(
                formatter,
                "post-action projection {projected_after_action_micro_inr} micro-INR is below current projection {current_micro_inr} micro-INR"
            ),
            Self::ProjectionOverflow => {
                formatter.write_str("safety-margin projection exceeds integer micro-INR capacity")
            }
        }
    }
}

impl std::error::Error for BudgetPolicyError {}

/// Apply a basis-point safety margin with conservative ceiling rounding.
pub fn apply_safety_margin(
    projected_micro_inr: u64,
    safety_margin_basis_points: u16,
) -> Result<u64, BudgetPolicyError> {
    let multiplier = BASIS_POINTS + u128::from(safety_margin_basis_points);
    let scaled = u128::from(projected_micro_inr) * multiplier;
    let adjusted = scaled.div_ceil(BASIS_POINTS);
    u64::try_from(adjusted).map_err(|_| BudgetPolicyError::ProjectionOverflow)
}

/// Evaluate one action against validated, per-environment effective projections.
pub fn evaluate_budget(
    thresholds: BudgetThresholds,
    evaluation: BudgetEvaluation,
) -> BudgetDecision {
    let projected = evaluation.projected_after_action_micro_inr;
    let band = if projected >= thresholds.hard_cap_micro_inr {
        BudgetBand::HardCap
    } else if projected > thresholds.workflow_spend_limit_micro_inr {
        BudgetBand::OperationalReserve
    } else if projected >= thresholds.suspension_micro_inr {
        BudgetBand::IntakeSuspended
    } else if projected >= thresholds.warning_micro_inr {
        BudgetBand::Warning
    } else {
        BudgetBand::Normal
    };

    let authorization = match (band, evaluation.action) {
        (BudgetBand::HardCap, _) => BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached),
        (
            BudgetBand::OperationalReserve,
            BudgetAction::ConfirmedOperation {
                funding: ConfirmedOperationFunding::RequiresNewReservation,
            },
        ) => BudgetAuthorization::Deny(BudgetDenialReason::ProtectedOperationalReserve),
        (BudgetBand::OperationalReserve, BudgetAction::NewWorkflowIntake) => {
            BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
        }
        (BudgetBand::OperationalReserve, BudgetAction::Deployment) => {
            BudgetAuthorization::Deny(BudgetDenialReason::DeploymentWhileIntakeSuspended)
        }
        (BudgetBand::IntakeSuspended, BudgetAction::NewWorkflowIntake) => {
            BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
        }
        (BudgetBand::IntakeSuspended, BudgetAction::Deployment) => {
            BudgetAuthorization::Deny(BudgetDenialReason::DeploymentWhileIntakeSuspended)
        }
        (
            BudgetBand::Normal
            | BudgetBand::Warning
            | BudgetBand::IntakeSuspended
            | BudgetBand::OperationalReserve,
            _,
        ) => BudgetAuthorization::Permit,
    };

    BudgetDecision {
        band,
        authorization,
        warning_crossed: evaluation.current_effective_projected_micro_inr
            < thresholds.warning_micro_inr
            && projected >= thresholds.warning_micro_inr,
        projected_after_action_micro_inr: projected,
    }
}
