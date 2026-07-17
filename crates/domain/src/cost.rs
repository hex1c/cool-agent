use std::fmt::{Display, Formatter};

/// Validated per-environment budget boundaries, expressed in integer micro-INR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetThresholds {
    warning_micro_inr: u64,
    suspension_micro_inr: u64,
    hard_cap_micro_inr: u64,
}

impl BudgetThresholds {
    pub fn new(
        warning_micro_inr: u64,
        suspension_micro_inr: u64,
        hard_cap_micro_inr: u64,
    ) -> Result<Self, BudgetPolicyError> {
        if warning_micro_inr >= suspension_micro_inr || suspension_micro_inr >= hard_cap_micro_inr {
            return Err(BudgetPolicyError::InvalidThresholdOrder);
        }
        Ok(Self {
            warning_micro_inr,
            suspension_micro_inr,
            hard_cap_micro_inr,
        })
    }
}

/// The operation for which the caller needs a budget authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAction {
    NewWorkflowIntake,
    ConfirmedOperation,
    Status,
    ArtifactRetrieval,
    Deployment,
}

/// Trusted effective projections before and after one action.
///
/// The values must already include reservations, reconciliation tightening, and
/// any approved safety margin. Deriving those values belongs outside this pure
/// threshold policy.
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
    HardCap,
}

/// Why a requested action was denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDenialReason {
    IntakeSuspended,
    DeploymentWhileIntakeSuspended,
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
    ProjectionDecreased {
        current_micro_inr: u64,
        projected_after_action_micro_inr: u64,
    },
}

impl Display for BudgetPolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidThresholdOrder => formatter
                .write_str("budget thresholds must be ordered warning < suspension < hard cap"),
            Self::ProjectionDecreased {
                current_micro_inr,
                projected_after_action_micro_inr,
            } => write!(
                formatter,
                "post-action projection {projected_after_action_micro_inr} micro-INR is below current projection {current_micro_inr} micro-INR"
            ),
        }
    }
}

impl std::error::Error for BudgetPolicyError {}

/// Evaluate one action against validated, per-environment effective projections.
pub fn evaluate_budget(
    thresholds: BudgetThresholds,
    evaluation: BudgetEvaluation,
) -> BudgetDecision {
    let projected = evaluation.projected_after_action_micro_inr;
    let band = if projected >= thresholds.hard_cap_micro_inr {
        BudgetBand::HardCap
    } else if projected >= thresholds.suspension_micro_inr {
        BudgetBand::IntakeSuspended
    } else if projected >= thresholds.warning_micro_inr {
        BudgetBand::Warning
    } else {
        BudgetBand::Normal
    };

    let authorization = match (band, evaluation.action) {
        (BudgetBand::HardCap, _) => BudgetAuthorization::Deny(BudgetDenialReason::HardCapReached),
        (BudgetBand::IntakeSuspended, BudgetAction::NewWorkflowIntake) => {
            BudgetAuthorization::Deny(BudgetDenialReason::IntakeSuspended)
        }
        (BudgetBand::IntakeSuspended, BudgetAction::Deployment) => {
            BudgetAuthorization::Deny(BudgetDenialReason::DeploymentWhileIntakeSuspended)
        }
        (BudgetBand::Normal | BudgetBand::Warning | BudgetBand::IntakeSuspended, _) => {
            BudgetAuthorization::Permit
        }
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
