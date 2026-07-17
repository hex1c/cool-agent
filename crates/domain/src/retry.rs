use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

pub const MAX_EXTERNAL_OPERATION_ATTEMPTS: u8 = 3;
const BASIS_POINTS: u64 = 10_000;

/// One-based physical attempt number for an external operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttemptNumber(u8);

impl AttemptNumber {
    pub fn new(value: u8) -> Result<Self, RetryPolicyError> {
        if value == 0 || value > MAX_EXTERNAL_OPERATION_ATTEMPTS {
            return Err(RetryPolicyError::InvalidAttempt { requested: value });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Deterministic random sample in the inclusive range -100% through +100%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JitterSample(i16);

impl JitterSample {
    pub fn new(basis_points: i16) -> Result<Self, RetryPolicyError> {
        if !(-10_000..=10_000).contains(&basis_points) {
            return Err(RetryPolicyError::InvalidJitterSample { basis_points });
        }
        Ok(Self(basis_points))
    }

    pub const fn basis_points(self) -> i16 {
        self.0
    }
}

/// Validated retry limits shared by all external operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u8,
    base_delay_ms: u32,
    maximum_delay_ms: u32,
    jitter_basis_points: u16,
}

impl RetryPolicy {
    pub fn new(
        max_attempts: u8,
        base_delay_ms: u32,
        maximum_delay_ms: u32,
        jitter_basis_points: u16,
    ) -> Result<Self, RetryPolicyError> {
        if max_attempts == 0 {
            return Err(RetryPolicyError::ZeroAttempts);
        }
        if max_attempts > MAX_EXTERNAL_OPERATION_ATTEMPTS {
            return Err(RetryPolicyError::TooManyAttempts {
                requested: max_attempts,
            });
        }
        if base_delay_ms == 0 {
            return Err(RetryPolicyError::ZeroBaseDelay);
        }
        if maximum_delay_ms < base_delay_ms {
            return Err(RetryPolicyError::MaximumBelowBase {
                base_delay_ms,
                maximum_delay_ms,
            });
        }
        if jitter_basis_points > 10_000 {
            return Err(RetryPolicyError::InvalidJitterBasisPoints {
                basis_points: jitter_basis_points,
            });
        }
        Ok(Self {
            max_attempts,
            base_delay_ms,
            maximum_delay_ms,
            jitter_basis_points,
        })
    }

    pub const fn max_attempts(self) -> u8 {
        self.max_attempts
    }

    pub fn after_failure(
        self,
        completed_attempt: AttemptNumber,
        jitter: JitterSample,
    ) -> Result<RetryDecision, RetryPolicyError> {
        if completed_attempt.get() >= self.max_attempts {
            return Ok(RetryDecision::Exhausted);
        }

        let next_attempt_value = completed_attempt.get().saturating_add(1);
        let next_attempt = AttemptNumber::new(next_attempt_value)?;
        let exponent = u32::from(completed_attempt.get().saturating_sub(1));
        let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        let nominal = u64::from(self.base_delay_ms)
            .saturating_mul(multiplier)
            .min(u64::from(self.maximum_delay_ms));
        let maximum_adjustment =
            nominal.saturating_mul(u64::from(self.jitter_basis_points)) / BASIS_POINTS;
        let sample_magnitude = u64::from(jitter.basis_points().unsigned_abs());
        let adjustment = maximum_adjustment.saturating_mul(sample_magnitude) / BASIS_POINTS;
        let jittered = if jitter.basis_points().is_negative() {
            nominal.saturating_sub(adjustment)
        } else {
            nominal.saturating_add(adjustment)
        }
        .min(u64::from(self.maximum_delay_ms));
        let delay_ms = u32::try_from(jittered).map_err(|_| RetryPolicyError::DelayOverflow)?;

        Ok(RetryDecision::Retry {
            next_attempt,
            delay_ms,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    Retry {
        next_attempt: AttemptNumber,
        delay_ms: u32,
    },
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryPolicyError {
    ZeroAttempts,
    TooManyAttempts {
        requested: u8,
    },
    InvalidAttempt {
        requested: u8,
    },
    ZeroBaseDelay,
    MaximumBelowBase {
        base_delay_ms: u32,
        maximum_delay_ms: u32,
    },
    InvalidJitterBasisPoints {
        basis_points: u16,
    },
    InvalidJitterSample {
        basis_points: i16,
    },
    DelayOverflow,
}

impl Display for RetryPolicyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid retry policy: {self:?}")
    }
}

impl std::error::Error for RetryPolicyError {}
